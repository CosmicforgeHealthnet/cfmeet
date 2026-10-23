//! The customer ingress pipeline.
//!
//! 1. New server-generated request and correlation ids (client ids are never adopted).
//! 2. Route lookup in the generated catalogue: no route is NOT_FOUND (default deny)
//!    BEFORE any credential is looked at; a known path with another method is
//!    METHOD_NOT_ALLOWED (with `Allow`).
//! 3. A client `X-CF-Service-Authorization` is refused (a service token never
//!    authenticates a customer); every other client authority header is dropped — the
//!    upstream request is rebuilt from an allow-list.
//! 4. The authentication mode of the request, from the route's modes and the presented
//!    `Authorization`:
//!    * PLATFORM_CREDENTIAL (`Bearer cfmk_…`): verified by Platform Access (rejected 401,
//!      throttled 429, unreachable 503, SAFETY_ONLY 401); the route scope is pre-checked;
//!      the platform request quota is enforced; the upstream receives a fresh 60-second
//!      service token carrying the delegated context derived from the authority chain
//!      (with the credential id, which the receiver re-verifies) — never the secret;
//!    * DEVELOPER_SESSION: the CF Meet session is transported to the Developer Platform
//!      (`X-CF-User-Session`, inside a gateway-signed request) for the session bridge;
//!    * domain end-user / device tokens (client tokens, realtime tickets, signaling
//!      credentials, meeting sessions, participant tokens, anonymous): transported
//!      verbatim to the owning service, which authenticates them. A platform credential
//!      is NEVER forwarded to a service (refused on routes that do not take one).
//! 5. Bounded body, per-route deadline, safe upstream errors; the owning service keeps
//!    final resource authorization; its status, allow-listed headers and body (streamed)
//!    are relayed.
//!
//! Logs carry the route id and ids, never a credential, token, body or header value.

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, request::Parts, HeaderMap, HeaderName, HeaderValue, Method},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use cf_service_auth::OutboundServiceRequest;
use futures_util::TryStreamExt;
use tracing::Instrument;

use super::error::{ApiError, ErrorCode};
use crate::{
    context::{scoped, RequestContext},
    headers::{
        presents_service_token, safe_value, stripped_authority_headers, FORWARDED_REQUEST_HEADERS,
        RELAYED_RESPONSE_HEADERS,
    },
    identity::{AuthenticatedCustomer, Authentication},
    quota::{Decision, Window},
    routing::{AuthMode, Lookup, Protocol, RouteMatch},
    state::AppState,
};

pub const USER_SESSION_HEADER: &str = "x-cf-user-session";
pub const CREDENTIAL_PREFIX: &str = "cfmk_";

pub fn with_ids(mut response: Response, context: &RequestContext) -> Response {
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

/// The presented `Authorization` (exactly one header, bounded, visible ASCII).
#[derive(Debug, Clone)]
pub struct Presented {
    pub scheme: String,
    pub token: String,
    pub raw: HeaderValue,
}

pub fn presented(headers: &HeaderMap) -> Result<Option<Presented>, ApiError> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() || value.len() > 4096 || !safe_value(value) {
        return Err(ApiError::Code(ErrorCode::AuthenticationFailed));
    }
    let text = value
        .to_str()
        .map_err(|_| ApiError::Code(ErrorCode::AuthenticationFailed))?;
    let (scheme, token) = text
        .split_once(' ')
        .ok_or(ApiError::Code(ErrorCode::AuthenticationFailed))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(ApiError::Code(ErrorCode::AuthenticationFailed));
    }
    Ok(Some(Presented {
        scheme: scheme.to_string(),
        token: token.to_string(),
        raw: value.clone(),
    }))
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// The raw query, forwarded as-is when it is bounded visible ASCII.
pub fn valid_query(query: Option<&str>) -> Result<Option<&str>, ApiError> {
    match query {
        None | Some("") => Ok(None),
        Some(q)
            if q.len() <= 2048 && q.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'#') =>
        {
            Ok(Some(q))
        }
        Some(_) => Err(ApiError::Code(ErrorCode::ValidationError)),
    }
}

/// How the request is authenticated and forwarded.
pub enum Forward {
    Credential(Box<AuthenticatedCustomer>),
    Session(String),
    Passthrough(AuthMode),
}

/// Chooses the authentication mode (see the module documentation).
pub fn select_mode(
    route: &RouteMatch<'_>,
    auth: Option<&Presented>,
    headers: &HeaderMap,
) -> Result<AuthMode, ApiError> {
    let d = route.definition;
    match auth {
        Some(p) if p.token.starts_with(CREDENTIAL_PREFIX) => {
            if p.scheme.eq_ignore_ascii_case("bearer") && d.allows(AuthMode::PlatformCredential) {
                Ok(AuthMode::PlatformCredential)
            } else {
                // A platform credential is never transported to a service.
                Err(ApiError::Code(ErrorCode::AuthenticationFailed))
            }
        }
        Some(p) if p.scheme.eq_ignore_ascii_case("cf-client-token") => d
            .allows(AuthMode::ClientToken)
            .then_some(AuthMode::ClientToken)
            .ok_or(ApiError::Code(ErrorCode::AuthenticationFailed)),
        Some(p) if p.scheme.eq_ignore_ascii_case("bearer") => {
            if d.allows(AuthMode::DeveloperSession) {
                Ok(AuthMode::DeveloperSession)
            } else if d.allows(AuthMode::SignalingCredential) && p.token.starts_with("cfsig_") {
                Ok(AuthMode::SignalingCredential)
            } else if d.allows(AuthMode::MeetingSession) {
                Ok(AuthMode::MeetingSession)
            } else {
                Err(ApiError::Code(ErrorCode::AuthenticationFailed))
            }
        }
        Some(_) => Err(ApiError::Code(ErrorCode::AuthenticationFailed)),
        None => {
            let passthrough = |m: AuthMode| d.allows(m);
            if headers.contains_key("x-participant-token")
                && passthrough(AuthMode::ParticipantToken)
            {
                return Ok(AuthMode::ParticipantToken);
            }
            if d.protocol == Protocol::WebSocket {
                // Tickets / credentials travel in the subprotocol or the query.
                if let Some(m) = d.auth.iter().copied().find(|m| m.is_passthrough()) {
                    return Ok(m);
                }
            }
            if passthrough(AuthMode::Anonymous) {
                return Ok(AuthMode::Anonymous);
            }
            // Meeting routes resolve anonymous callers themselves (invitation holders,
            // new guests); the owning service decides.
            if passthrough(AuthMode::MeetingSession) {
                return Ok(AuthMode::MeetingSession);
            }
            Err(ApiError::Code(ErrorCode::AuthenticationFailed))
        }
    }
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
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path().to_string();
    let route = match state.routes.lookup(&parts.method, &path) {
        Lookup::Matched(m) => m,
        Lookup::MethodNotAllowed(methods) => {
            let mut r = ApiError::Code(ErrorCode::MethodNotAllowed).into_response();
            let allow = methods
                .iter()
                .map(Method::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            if let Ok(v) = HeaderValue::from_str(&allow) {
                r.headers_mut().insert(header::ALLOW, v);
            }
            return r;
        }
        Lookup::NoRoute => {
            tracing::debug!("no route (default deny)");
            return ApiError::Code(ErrorCode::NotFound).into_response();
        }
    };
    let route_id = route.definition.id.clone();
    tracing::Span::current().record("route", route_id.as_str());
    if presents_service_token(&parts.headers) {
        tracing::warn!(route = %route_id, "client presented a service token at the ingress");
        return ApiError::Code(ErrorCode::AuthenticationFailed).into_response();
    }
    let stripped = stripped_authority_headers(&parts.headers);
    if !stripped.is_empty() {
        tracing::info!(route = %route_id, headers = ?stripped, "client authority headers dropped");
    }
    let auth = match presented(&parts.headers) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let mode = match select_mode(&route, auth.as_ref(), &parts.headers) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let upgrade = is_websocket_upgrade(&parts.headers);
    if route.definition.protocol == Protocol::WebSocket {
        if !upgrade || parts.method != Method::GET {
            return ApiError::Code(ErrorCode::ValidationError).into_response();
        }
        if !mode.is_passthrough() {
            return ApiError::Code(ErrorCode::ProtocolNotEnabled).into_response();
        }
        return crate::ws::proxy(&state, &route, &mut parts, auth.as_ref(), mode).await;
    }
    if upgrade {
        return ApiError::Code(ErrorCode::ValidationError).into_response();
    }
    let forward = match mode {
        AuthMode::PlatformCredential => {
            let token = auth.as_ref().map(|a| a.token.clone()).unwrap_or_default();
            let customer = match authenticate(&state, &token, &context, &route_id).await {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };
            if let Some(scope) = &route.definition.required_scope {
                if !customer.has_scope(scope) {
                    tracing::info!(route = %route_id, scope = %scope, "scope check denied");
                    return ApiError::Code(ErrorCode::InsufficientScope).into_response();
                }
            }
            Forward::Credential(customer)
        }
        AuthMode::DeveloperSession => {
            Forward::Session(auth.as_ref().map(|a| a.token.clone()).unwrap_or_default())
        }
        m => Forward::Passthrough(m),
    };
    // Platform request quotas (authenticated credentials only).
    let mut quota_headers = Vec::new();
    if let (Forward::Credential(customer), Some(engine)) = (&forward, &state.quota) {
        match engine
            .check(
                customer.environment_id,
                customer.environment_kind,
                &context.correlation_id,
            )
            .await
        {
            Decision::Allowed {
                limits,
                remaining_minute,
                reset_seconds,
            } => {
                quota_headers.push(("x-cf-quota-limit-minute", limits.per_minute.to_string()));
                quota_headers.push(("x-cf-quota-remaining-minute", remaining_minute.to_string()));
                quota_headers.push(("x-cf-quota-reset", reset_seconds.to_string()));
                quota_headers.push(("x-cf-quota-limit-day", limits.per_day.to_string()));
            }
            Decision::Exceeded {
                window,
                level,
                retry_after_seconds,
                ..
            } => {
                tracing::info!(route = %route_id, ?window, level, "platform request quota exceeded");
                let mut r = ApiError::QuotaExceeded {
                    retry_after_seconds,
                }
                .into_response();
                let w = match window {
                    Window::Minute => "requests-per-minute",
                    Window::Day => "requests-per-day",
                };
                r.headers_mut()
                    .insert("x-cf-quota-window", HeaderValue::from_static(w));
                r.headers_mut()
                    .insert("x-cf-quota-level", HeaderValue::from_static(level));
                return r;
            }
        }
    }
    let bytes = match to_bytes(body, state.max_body_bytes).await {
        Ok(b) => b,
        Err(_) => return ApiError::Code(ErrorCode::PayloadTooLarge).into_response(),
    };
    if !bytes.is_empty() && matches!(parts.method, Method::GET | Method::HEAD) {
        return ApiError::Code(ErrorCode::ValidationError).into_response();
    }
    let mut response = forward_rest(&state, &context, &route, &parts, bytes, forward).await;
    for (k, v) in quota_headers {
        if let Ok(v) = HeaderValue::from_str(&v) {
            response.headers_mut().insert(k, v);
        }
    }
    response
}

pub async fn authenticate(
    state: &AppState,
    presented: &str,
    context: &RequestContext,
    route_id: &str,
) -> Result<Box<AuthenticatedCustomer>, ApiError> {
    match state
        .authority
        .authenticate(presented, &context.correlation_id)
        .await
    {
        Ok(Authentication::Authenticated(c)) if c.full_authority => Ok(c),
        Ok(Authentication::Authenticated(_)) => {
            tracing::info!(route = route_id, "authentication refused (suspended scope)");
            Err(ApiError::Code(ErrorCode::AuthenticationFailed))
        }
        Ok(Authentication::Rejected { reason, throttle }) => {
            tracing::info!(route = route_id, %reason, "authentication rejected");
            Err(match throttle {
                Some(t) => ApiError::RateLimited {
                    retry_after_seconds: t.retry_after_seconds,
                },
                None => ApiError::Code(ErrorCode::AuthenticationFailed),
            })
        }
        Err(e) => {
            tracing::warn!(route = route_id, error = %e, "identity authority unavailable");
            Err(ApiError::Code(ErrorCode::ServiceUnavailable))
        }
    }
}

/// Client headers forwarded on `route` in `mode` (allow-list only).
pub fn forwarded_headers(
    parts: &Parts,
    route: &RouteMatch<'_>,
    mode: Option<AuthMode>,
    auth: Option<&Presented>,
) -> HeaderMap {
    let mut out = HeaderMap::new();
    let extra = route.definition.forward_headers.iter().map(String::as_str);
    for name in FORWARDED_REQUEST_HEADERS.iter().copied().chain(extra) {
        if let Some(v) = parts.headers.get(name).filter(|v| safe_value(v)) {
            if let Ok(n) = HeaderName::from_bytes(name.as_bytes()) {
                out.insert(n, v.clone());
            }
        }
    }
    match mode {
        Some(AuthMode::ClientToken | AuthMode::SignalingCredential | AuthMode::MeetingSession) => {
            if let Some(a) = auth {
                out.insert(header::AUTHORIZATION, a.raw.clone());
            }
        }
        Some(AuthMode::ParticipantToken) => {
            if let Some(v) = parts
                .headers
                .get("x-participant-token")
                .filter(|v| safe_value(v))
            {
                out.insert(HeaderName::from_static("x-participant-token"), v.clone());
            }
            if let Some(a) = auth {
                out.insert(header::AUTHORIZATION, a.raw.clone());
            }
        }
        _ => {}
    }
    out
}

pub fn upstream_url(
    state: &AppState,
    route: &RouteMatch<'_>,
    query: Option<&str>,
) -> Option<String> {
    let base = state.service_auth.upstream_base(route.upstream)?;
    let mut url = format!(
        "{}{}",
        base.as_str().trim_end_matches('/'),
        route.upstream_path
    );
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    Some(url)
}

async fn forward_rest(
    state: &AppState,
    context: &RequestContext,
    route: &RouteMatch<'_>,
    parts: &Parts,
    body: Bytes,
    forward: Forward,
) -> Response {
    let route_id = route.definition.id.as_str();
    let query = match valid_query(parts.uri.query()) {
        Ok(q) => q,
        Err(e) => return e.into_response(),
    };
    let Some(url) = upstream_url(state, route, query) else {
        tracing::warn!(route = route_id, upstream = %route.upstream, "upstream not configured");
        return ApiError::Code(ErrorCode::ServiceUnavailable).into_response();
    };
    let mode = match &forward {
        Forward::Passthrough(m) => Some(*m),
        _ => None,
    };
    let auth = presented(&parts.headers).ok().flatten();
    let mut headers = forwarded_headers(parts, route, mode, auth.as_ref());
    match &forward {
        Forward::Credential(customer) => {
            let hop =
                OutboundServiceRequest::continuing(route.upstream.clone(), &context.correlation_id)
                    .with_delegated_context(customer.delegated());
            match state.service_auth.signer().sign(&hop) {
                Ok(signed) => signed.apply_to(&mut headers),
                Err(_) => return ApiError::Code(ErrorCode::InternalError).into_response(),
            }
        }
        Forward::Session(token) => {
            let hop =
                OutboundServiceRequest::continuing(route.upstream.clone(), &context.correlation_id);
            match (
                state.service_auth.signer().sign(&hop),
                HeaderValue::from_str(token),
            ) {
                (Ok(signed), Ok(session)) => {
                    signed.apply_to(&mut headers);
                    headers.insert(HeaderName::from_static(USER_SESSION_HEADER), session);
                }
                _ => return ApiError::Code(ErrorCode::AuthenticationFailed).into_response(),
            }
        }
        Forward::Passthrough(_) => {}
    }
    let deadline = route.definition.timeout.unwrap_or(state.upstream_timeout);
    let mut req = state
        .http
        .request(parts.method.clone(), &url)
        .headers(headers)
        .timeout(deadline);
    if !body.is_empty() {
        req = req.body(body);
    }
    let upstream = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                route = route_id,
                timeout = e.is_timeout(),
                connect = e.is_connect(),
                "upstream call failed"
            );
            return ApiError::Code(if e.is_timeout() {
                ErrorCode::UpstreamTimeout
            } else {
                ErrorCode::UpstreamUnavailable
            })
            .into_response();
        }
    };
    let status = upstream.status();
    tracing::info!(route = route_id, status = status.as_u16(), "forwarded");
    let mut builder = Response::builder().status(status.as_u16());
    let mut has_cache_control = false;
    for name in RELAYED_RESPONSE_HEADERS {
        for v in upstream.headers().get_all(*name) {
            if safe_value(v) {
                has_cache_control |= *name == "cache-control";
                builder = builder.header(*name, v.clone());
            }
        }
    }
    if !has_cache_control {
        builder = builder.header(header::CACHE_CONTROL, "no-store");
    }
    let stream = upstream
        .bytes_stream()
        .map_err(|e| std::io::Error::other(e.to_string()));
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| ApiError::Code(ErrorCode::InternalError).into_response())
}
