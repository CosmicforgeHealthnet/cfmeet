//! The customer ingress pipeline.
//!
//! 1. New server-generated request and correlation ids (client ids are never adopted).
//! 2. Route lookup: no route is NOT_FOUND (default deny) BEFORE any credential is looked
//!    at; a known path with another method is METHOD_NOT_ALLOWED.
//! 3. A client `X-CF-Service-Authorization` is refused (a service token never
//!    authenticates a customer); every other client authority header is dropped.
//! 4. `Authorization: Bearer <credential>` verified by Platform Access (rejected: 401,
//!    throttled: 429, unreachable: 503, SAFETY_ONLY: 401).
//! 5. The route's scope (Platform Access scopes only).
//! 6. Protocol: WebSocket forwarding is Section 51 (PROTOCOL_NOT_ENABLED).
//! 7. Bounded JSON body; forwarded with a fresh short-lived service token for the route's
//!    upstream carrying the delegated context derived from the authority chain — never
//!    from the request — under the per-route deadline. The upstream keeps final resource
//!    authorization; its status and JSON body are relayed, internal headers are not.
//!
//! Logs carry the route id and ids, never a credential, token, body or client header value.

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use cf_service_auth::{client::InternalCallError, OutboundServiceRequest};
use serde_json::Value;
use tracing::Instrument;

use super::error::{ApiError, ErrorCode};
use crate::{
    context::{scoped, RequestContext},
    headers::{presents_service_token, stripped_authority_headers, FORWARDED_CLIENT_HEADERS},
    identity::Authentication,
    routing::{Lookup, Protocol},
    state::AppState,
};

fn with_ids(mut response: Response, context: &RequestContext) -> Response {
    for (name, value) in [
        ("x-request-id", context.request_id.as_str()),
        ("x-correlation-id", context.correlation_id.as_str()),
    ] {
        if let Ok(v) = HeaderValue::from_str(value) {
            response.headers_mut().insert(name, v);
        }
    }
    response
}

/// `Authorization: Bearer <value>` (exactly one header, bounded).
fn bearer(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > 1_024 {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .then(|| token.trim().to_string())
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// Query pairs (at most 16; the internal client validates keys and values again).
fn query_pairs(query: Option<&str>) -> Option<Vec<(String, String)>> {
    let Some(q) = query.filter(|q| !q.is_empty()) else {
        return Some(Vec::new());
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    (pairs.len() <= 16).then_some(pairs)
}

pub async fn ingress(State(state): State<AppState>, request: Request) -> Response {
    let context = RequestContext::generate();
    let span = tracing::info_span!("ingress", request_id = %context.request_id,
        correlation_id = %context.correlation_id, method = %request.method(),
        route = tracing::field::Empty);
    let response = scoped(
        context.clone(),
        handle(state, request, context.clone()).instrument(span),
    )
    .await;
    with_ids(response, &context)
}

async fn handle(state: AppState, request: Request, context: RequestContext) -> Response {
    let (parts, body) = request.into_parts();
    let route = match state.routes.lookup(&parts.method, parts.uri.path()) {
        Lookup::Matched(m) => m,
        Lookup::MethodNotAllowed => {
            return ApiError::Code(ErrorCode::MethodNotAllowed).into_response()
        }
        Lookup::NoRoute => {
            tracing::debug!("no route (default deny)");
            return ApiError::Code(ErrorCode::NotFound).into_response();
        }
    };
    let route_id = route.definition.id;
    tracing::Span::current().record("route", route_id);
    if presents_service_token(&parts.headers) {
        tracing::warn!(
            route = route_id,
            "client presented a service token at the ingress"
        );
        return ApiError::Code(ErrorCode::AuthenticationFailed).into_response();
    }
    let stripped = stripped_authority_headers(&parts.headers);
    if !stripped.is_empty() {
        tracing::info!(route = route_id, headers = ?stripped, "client authority headers dropped");
    }
    let Some(presented) = bearer(&parts.headers) else {
        return ApiError::Code(ErrorCode::AuthenticationFailed).into_response();
    };
    let customer = match state
        .authority
        .authenticate(&presented, &context.correlation_id)
        .await
    {
        Ok(Authentication::Authenticated(c)) if c.full_authority => c,
        Ok(Authentication::Authenticated(_)) => {
            tracing::info!(route = route_id, "authentication refused (suspended scope)");
            return ApiError::Code(ErrorCode::AuthenticationFailed).into_response();
        }
        Ok(Authentication::Rejected { reason, throttle }) => {
            tracing::info!(route = route_id, %reason, "authentication rejected");
            return match throttle {
                Some(t) => ApiError::RateLimited {
                    retry_after_seconds: t.retry_after_seconds,
                }
                .into_response(),
                None => ApiError::Code(ErrorCode::AuthenticationFailed).into_response(),
            };
        }
        Err(e) => {
            tracing::warn!(route = route_id, error = %e, "identity authority unavailable");
            return ApiError::Code(ErrorCode::ServiceUnavailable).into_response();
        }
    };
    if let Some(scope) = route.definition.required_scope {
        if !customer.has_scope(scope) {
            tracing::info!(route = route_id, scope, "scope check denied");
            return ApiError::Code(ErrorCode::InsufficientScope).into_response();
        }
    }
    if route.definition.protocol == Protocol::WebSocket || is_websocket_upgrade(&parts.headers) {
        return ApiError::Code(ErrorCode::ProtocolNotEnabled).into_response();
    }
    let bytes = match to_bytes(body, state.max_body_bytes).await {
        Ok(b) => b,
        Err(_) => return ApiError::Code(ErrorCode::PayloadTooLarge).into_response(),
    };
    let json_body: Option<Value> = if bytes.is_empty() {
        None
    } else {
        let is_json = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        match serde_json::from_slice(&bytes) {
            Ok(v) if is_json => Some(v),
            _ => return ApiError::Code(ErrorCode::ValidationError).into_response(),
        }
    };
    if json_body.is_some() && matches!(parts.method, Method::GET | Method::HEAD) {
        return ApiError::Code(ErrorCode::ValidationError).into_response();
    }
    let Some(query) = query_pairs(parts.uri.query()) else {
        return ApiError::Code(ErrorCode::ValidationError).into_response();
    };
    let query: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut forwarded: Vec<(&str, &str)> = Vec::new();
    for name in FORWARDED_CLIENT_HEADERS {
        if let Some(v) = parts.headers.get(*name).and_then(|v| v.to_str().ok()) {
            forwarded.push((name, v));
        }
    }
    // The ONLY authority upstream: the signed delegated context from Platform Access.
    let hop = OutboundServiceRequest::continuing(route.upstream.clone(), &context.correlation_id)
        .with_delegated_context(customer.delegated());
    let deadline = route.definition.timeout.unwrap_or(state.upstream_timeout);
    let client = state.service_auth.client();
    let call = client.exchange_json_query_with_headers(
        &hop,
        parts.method.clone(),
        &route.upstream_path,
        &query,
        &forwarded,
        json_body.as_ref(),
    );
    let upstream = match tokio::time::timeout(deadline, call).await {
        Err(_) => {
            tracing::warn!(route = route_id, "upstream deadline exceeded");
            return ApiError::Code(ErrorCode::UpstreamTimeout).into_response();
        }
        Ok(Err(e)) => {
            tracing::warn!(route = route_id, error = %e, "upstream call failed");
            return ApiError::Code(match e {
                InternalCallError::Timeout { .. } => ErrorCode::UpstreamTimeout,
                InternalCallError::Connect { .. } => ErrorCode::UpstreamUnavailable,
                InternalCallError::UnknownTarget(_) => ErrorCode::ServiceUnavailable,
                InternalCallError::InvalidHeader | InternalCallError::InvalidPath => {
                    ErrorCode::ValidationError
                }
                _ => ErrorCode::UpstreamUnavailable,
            })
            .into_response();
        }
        Ok(Ok(r)) => r,
    };
    tracing::info!(route = route_id, status = upstream.status(), upstream_request_id = %upstream.request_id(), "forwarded");
    relay(&upstream)
}

/// The upstream's status and JSON body. Upstream headers are not relayed except the
/// internal client's allow-list (`Idempotent-Replayed`); responses are `no-store`.
fn relay(upstream: &cf_service_auth::client::InternalResponse) -> Response {
    let status = StatusCode::from_u16(upstream.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let body = upstream.body();
    if !body.is_empty() && serde_json::from_slice::<serde::de::IgnoredAny>(body).is_err() {
        return ApiError::Code(ErrorCode::UpstreamUnavailable).into_response();
    }
    let mut response = Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, "no-store");
    if !body.is_empty() {
        response = response.header(header::CONTENT_TYPE, "application/json");
    }
    for name in cf_service_auth::client::RELAYED_RESPONSE_HEADERS {
        if let Some(v) = upstream.header(name) {
            response = response.header(name, v);
        }
    }
    response
        .body(Body::from(body.to_vec()))
        .unwrap_or_else(|_| ApiError::Code(ErrorCode::InternalError).into_response())
}
