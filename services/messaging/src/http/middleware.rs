//! Request context and authentication (Part V C.8).
//!
//! | Routes | Authentication |
//! |---|---|
//! | `/health`, `/ready`, `/openapi.json` | none (no data) |
//! | `/v1/**` | `Authorization: Bearer <CF Meet API credential>` verified by Platform Access ([`CustomerCaller`]); `GET` may use the bounded, feed-invalidated cache, every other method authenticates afresh. A `CF-Client-Token` scheme is refused explicitly (client tokens are a later workstream; there is no fake or permanent client token) |
//! | `/internal/v1/**` | an Ed25519 service token whose issuer Messaging's trust bundle grants the route's permission ([`InternalCaller`]) |
//! | operations | `GET /metrics` on a separate listener only |
//!
//! The ACTING principal ([`Acting`]) comes from `CF-Acting-Principal`, honoured only
//! inside the credential's principal restriction and when Platform Access resolves it
//! ACTIVE and messaging-capable in the credential's own environment — always fresh.
//!
//! Spans carry the matched ROUTE PATTERN, never the path; credentials, tokens, keys and
//! content are never logged.

use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers, InternalPermission, InternalServiceContext};
use tracing::Instrument;
use uuid::Uuid;

use super::error::{ApiError, ErrorCode};
use crate::{
    domain::{scoped, CausationId, RequestContext},
    identity::{cache::Freshness, Authentication, PrincipalStatus, TrustedContext},
    state::AppState,
};

/// Header naming the principal an application backend acts for.
pub const ACTING_PRINCIPAL: &str = "cf-acting-principal";

fn route_pattern(request: &Request) -> &str {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>")
}

fn with_request_id(mut response: Response, context: &RequestContext) -> Response {
    if let Ok(value) = HeaderValue::from_str(context.request_id.as_str()) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Public probe routes: a fresh server-generated request context.
pub async fn request_context(request: Request, next: Next) -> Response {
    let context = RequestContext::generate();
    let span = tracing::info_span!("request", request_id = %context.request_id, method = %request.method(),
        route = route_pattern(&request));
    let response = scoped(context.clone(), next.run(request).instrument(span)).await;
    with_request_id(response, &context)
}

/// The authenticated customer of a `/v1` request.
#[derive(Debug, Clone)]
pub struct CustomerCaller(pub TrustedContext);

impl CustomerCaller {
    /// Fails with INSUFFICIENT_SCOPE unless the credential holds `scope`.
    pub fn require_scope(&self, scope: &'static str) -> Result<&TrustedContext, ApiError> {
        if self.0.has_scope(scope) {
            Ok(&self.0)
        } else {
            crate::metrics::inc("messaging_scope_denied_total", &[("scope", scope)]);
            tracing::info!(scope, "scope check denied");
            Err(ApiError::code(ErrorCode::InsufficientScope))
        }
    }

    pub fn context(&self) -> &TrustedContext {
        &self.0
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for CustomerCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CustomerCaller>()
            .cloned()
            .ok_or(ApiError::code(ErrorCode::AuthenticationFailed))
    }
}

enum Presented {
    Credential(String),
    ClientToken(String),
    Invalid,
}

/// Part V §105: the member operations an end-user device may call DIRECTLY with its
/// client token (method, path pattern; `*` is any id segment, `{device}` must be the
/// session's own device). Everything else — conversation creation and administration,
/// membership management, policy, compliance, moderation, device registration and
/// revocation, client-token issuance — stays with the customer's backend credential.
const CLIENT_TOKEN_ROUTES: &[(&str, &str)] = &[
    ("GET", "/v1/conversations"),
    ("GET", "/v1/conversations/*"),
    ("GET", "/v1/conversations/*/members"),
    ("GET", "/v1/conversations/*/messages"),
    ("POST", "/v1/conversations/*/messages"),
    ("GET", "/v1/conversations/*/messages/*"),
    ("PATCH", "/v1/conversations/*/messages/*"),
    ("DELETE", "/v1/conversations/*/messages/*"),
    ("GET", "/v1/conversations/*/messages/*/revisions"),
    ("GET", "/v1/conversations/*/messages/*/reactions"),
    ("PUT", "/v1/conversations/*/messages/*/reactions/*"),
    ("DELETE", "/v1/conversations/*/messages/*/reactions/*"),
    ("GET", "/v1/conversations/*/messages/*/sms-transport"),
    ("PUT", "/v1/conversations/*/read-cursor"),
    ("GET", "/v1/conversations/*/events"),
    ("GET", "/v1/conversations/*/unread"),
    ("GET", "/v1/conversations/*/pins"),
    ("GET", "/v1/conversations/*/search"),
    ("POST", "/v1/conversations/*/reports"),
    ("POST", "/v1/conversations/*/mls/commits"),
    ("GET", "/v1/conversations/*/mls/state"),
    ("POST", "/v1/conversations/*/mls/key-package-claims"),
    ("POST", "/v1/conversations/*/attachments"),
    ("GET", "/v1/conversations/*/attachments/*"),
    ("POST", "/v1/conversations/*/attachments/*/finalize"),
    ("POST", "/v1/conversations/*/attachments/*/download-grants"),
    ("POST", "/v1/conversations/*/attachments/*/thumbnail-grants"),
    ("GET", "/v1/conversations/*/attachments/*/content"),
    ("GET", "/v1/messaging/search"),
    ("GET", "/v1/messaging/principals/*/devices"),
    ("GET", "/v1/messaging/key-transparency/proofs"),
    ("POST", "/v1/messaging/devices/{device}/key-packages"),
    ("GET", "/v1/messaging/devices/{device}/welcomes"),
    ("POST", "/v1/messaging/devices/{device}/welcomes/*/consume"),
    ("PUT", "/v1/messaging/devices/{device}/push-registration"),
    ("DELETE", "/v1/messaging/devices/{device}/push-registration"),
    ("GET", "/v1/messaging/blocks"),
    ("POST", "/v1/messaging/blocks"),
    ("DELETE", "/v1/messaging/blocks/*"),
    ("GET", "/v1/messaging/presence/settings"),
    ("PUT", "/v1/messaging/presence/settings"),
    ("POST", "/v1/messaging/history-transfers"),
    ("GET", "/v1/messaging/history-transfers"),
    ("GET", "/v1/messaging/history-transfers/*"),
    ("PUT", "/v1/messaging/history-transfers/*/sealed-history"),
    ("GET", "/v1/messaging/history-transfers/*/sealed-history"),
    ("POST", "/v1/messaging/history-transfers/*/complete"),
    ("POST", "/v1/messaging/history-transfers/*/cancel"),
    ("POST", "/v1/messaging/backups"),
    ("GET", "/v1/messaging/backups/latest"),
    ("DELETE", "/v1/messaging/backups/*"),
    ("POST", "/v1/messaging/backups/*/finalize"),
    ("POST", "/v1/messaging/backups/*/download-grants"),
];

/// Whether a client token may call `method path`; `Some(Some(device))` when the route
/// names a device that must be the session's own.
pub fn client_token_route(method: &Method, path: &str) -> Option<Option<String>> {
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    'routes: for (m, pattern) in CLIENT_TOKEN_ROUTES {
        if *m != method.as_str() {
            continue;
        }
        let want: Vec<&str> = pattern.split('/').collect();
        if want.len() != segments.len() {
            continue;
        }
        let mut device = None;
        for (w, got) in want.iter().zip(&segments) {
            match *w {
                "*" if !got.is_empty() => {}
                "{device}" if !got.is_empty() => device = Some(got.to_string()),
                literal if literal == *got => {}
                _ => continue 'routes,
            }
        }
        return Some(device);
    }
    None
}

/// Exactly one bounded `Authorization` header.
fn presented(request: &Request) -> Presented {
    let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return Presented::Invalid;
    };
    if values.next().is_some() || value.len() > 1_024 {
        return Presented::Invalid;
    }
    let Some((scheme, token)) = value.to_str().ok().and_then(|v| v.split_once(' ')) else {
        return Presented::Invalid;
    };
    let token = token.trim();
    if token.is_empty() {
        Presented::Invalid
    } else if scheme.eq_ignore_ascii_case("bearer") {
        Presented::Credential(token.to_string())
    } else if scheme.eq_ignore_ascii_case("cf-client-token") {
        Presented::ClientToken(token.to_string())
    } else {
        Presented::Invalid
    }
}

pub async fn require_customer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let context = RequestContext::generate();
    let span = tracing::info_span!("customer_request", request_id = %context.request_id,
        correlation_id = %context.correlation_id, method = %request.method(), route = route_pattern(&request));
    let response = scoped(
        context.clone(),
        async move {
            // A service token never authenticates a customer route.
            if request
                .headers()
                .contains_key(headers::SERVICE_AUTHORIZATION)
            {
                return ApiError::code(ErrorCode::AuthenticationFailed).into_response();
            }
            let token = match presented(&request) {
                Presented::Credential(t) => zeroize::Zeroizing::new(t),
                Presented::ClientToken(t) => {
                    let token = zeroize::Zeroizing::new(t);
                    return client_token_request(&state, &token, request, next).await;
                }
                Presented::Invalid => {
                    return ApiError::code(ErrorCode::AuthenticationFailed).into_response()
                }
            };
            let freshness = if request.method() == Method::GET || request.method() == Method::HEAD {
                Freshness::BoundedStale
            } else {
                Freshness::Fresh
            };
            match state.authority.authenticate_with(&token, freshness).await {
                Ok(Authentication::Authenticated(ctx)) if ctx.full_authority => {
                    drop(token);
                    let mutation =
                        request.method() != Method::GET && request.method() != Method::HEAD;
                    request.extensions_mut().insert(CustomerCaller(*ctx));
                    let response = next.run(request).await;
                    if mutation && response.status().is_success() {
                        // A committed change may have queued events: publish them now.
                        state.wake_outbox();
                    }
                    response
                }
                Ok(Authentication::Authenticated(_)) => {
                    tracing::info!("customer authentication refused (suspended scope)");
                    ApiError::code(ErrorCode::AuthoritySuspended).into_response()
                }
                Ok(Authentication::Rejected { reason, throttle }) => {
                    crate::metrics::inc("messaging_customer_auth_rejected_total", &[]);
                    tracing::info!(%reason, "customer authentication rejected");
                    match throttle {
                        Some(t) => ApiError::RateLimited {
                            retry_after_seconds: t.retry_after_seconds,
                        }
                        .into_response(),
                        None => ApiError::code(ErrorCode::AuthenticationFailed).into_response(),
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "identity authority unavailable");
                    ApiError::code(ErrorCode::ServiceUnavailable).into_response()
                }
            }
        }
        .instrument(span),
    )
    .await;
    with_request_id(response, &context)
}

/// A device-direct REST request with a client token (§105): only the allow-listed member
/// operations; the token's session (validated now: device ACTIVE, session ACTIVE and
/// unexpired, principal ACTIVE in Platform Access) becomes the trusted context — its
/// scopes, its environment, and its principal as the ONLY principal it may act for.
async fn client_token_request(
    state: &AppState,
    token: &str,
    mut request: Request,
    next: Next,
) -> Response {
    // A deployment without client-token keys accepts client tokens nowhere.
    if state.realtime.keys.is_none() {
        return ApiError::code(ErrorCode::ClientTokensNotEnabled).into_response();
    }
    let Some(device) = client_token_route(request.method(), request.uri().path()) else {
        crate::metrics::inc(
            "messaging_client_token_rest_total",
            &[("outcome", "route_not_allowed")],
        );
        return ApiError::code(ErrorCode::ClientTokensNotEnabled).into_response();
    };
    let session = match crate::realtime::sessions::authenticate(state, token).await {
        Ok(s) => s,
        Err(e) => {
            crate::metrics::inc(
                "messaging_client_token_rest_total",
                &[("outcome", "rejected")],
            );
            return e.into_response();
        }
    };
    // Own-device routes: another device of the same principal is NOT_FOUND.
    if device.is_some_and(|d| Uuid::parse_str(&d).ok() != Some(session.device_id)) {
        return ApiError::code(ErrorCode::NotFound).into_response();
    }
    match acting_header(request.headers()) {
        Ok(None) => {
            let value = HeaderValue::from_str(&session.principal_id.hyphenated().to_string())
                .expect("uuid is a valid header value");
            request.headers_mut().insert(ACTING_PRINCIPAL, value);
        }
        Ok(Some(p)) if p == session.principal_id => {}
        Ok(Some(_)) => {
            return ApiError::reason(
                ErrorCode::PrincipalNotAuthorized,
                "ACTING_PRINCIPAL_MISMATCH",
            )
            .into_response()
        }
        Err(e) => return e.into_response(),
    }
    let Some(ctx) = session.trusted_context() else {
        return ApiError::code(ErrorCode::AuthenticationFailed).into_response();
    };
    crate::metrics::inc(
        "messaging_client_token_rest_total",
        &[("outcome", "accepted")],
    );
    let mutation = request.method() != Method::GET && request.method() != Method::HEAD;
    request.extensions_mut().insert(CustomerCaller(ctx));
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        state.wake_outbox();
    }
    response
}

/// The principal an authenticated application backend acts for, verified now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acting {
    pub principal_id: Uuid,
    pub kind: String,
}

/// Parses `CF-Acting-Principal` (exactly one canonical UUID); `None` when absent.
pub fn acting_header(headers: &HeaderMap) -> Result<Option<Uuid>, ApiError> {
    let mut values = headers.get_all(ACTING_PRINCIPAL).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    let parsed = value.to_str().ok().and_then(|s| {
        Uuid::parse_str(s.trim())
            .ok()
            .filter(|u| u.hyphenated().to_string() == s.trim())
    });
    match (parsed, values.next()) {
        (Some(id), None) => Ok(Some(id)),
        _ => Err(ApiError::validation(
            "CF-Acting-Principal must be exactly one lower-case hyphenated UUID",
        )),
    }
}

/// Verifies that `ctx` may act for `principal_id` NOW: credential restriction, then
/// Platform Access resolution in the trusted scope (fresh), active and messaging-capable.
pub async fn verify_principal(
    state: &AppState,
    ctx: &TrustedContext,
    principal_id: Uuid,
) -> Result<Acting, ApiError> {
    if !ctx.may_act_for(principal_id) {
        crate::metrics::inc(
            "messaging_principal_denied_total",
            &[("reason", "restriction")],
        );
        return Err(ApiError::reason(
            ErrorCode::PrincipalNotAuthorized,
            "CREDENTIAL_RESTRICTION",
        ));
    }
    match state.principals.resolve(&ctx.scope, principal_id).await {
        Ok(status @ PrincipalStatus::Active { .. }) if status.is_messaging_capable() => {
            let PrincipalStatus::Active { kind } = status else {
                unreachable!()
            };
            Ok(Acting { principal_id, kind })
        }
        Ok(PrincipalStatus::Active { .. }) => Err(ApiError::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_KIND_NOT_MESSAGING",
        )),
        Ok(PrincipalStatus::Inactive) => Err(ApiError::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_INACTIVE",
        )),
        Ok(PrincipalStatus::NotFound) => Err(ApiError::reason(
            ErrorCode::PrincipalNotAuthorized,
            "PRINCIPAL_UNKNOWN",
        )),
        Err(e) => {
            tracing::warn!(error = %e, "principal directory unavailable");
            Err(ApiError::code(ErrorCode::ServiceUnavailable))
        }
    }
}

/// The required acting principal of a request.
pub async fn require_acting(
    state: &AppState,
    caller: &CustomerCaller,
    headers: &HeaderMap,
) -> Result<Acting, ApiError> {
    let id = acting_header(headers)?.ok_or(ApiError::code(ErrorCode::ActingPrincipalRequired))?;
    verify_principal(state, caller.context(), id).await
}

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct InternalCaller(InternalServiceContext);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// Fails with INTERNAL_PERMISSION_DENIED unless Messaging's trust bundle grants
    /// `permission` to the caller.
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.0.require_permission(permission).map_err(|denied| {
            tracing::warn!(caller = %denied.service, permission = denied.permission.as_str(), "internal permission denied");
            ApiError::code(ErrorCode::InternalPermissionDenied)
        })
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for InternalCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<InternalCaller>()
            .cloned()
            .ok_or(ApiError::code(ErrorCode::InternalAuthenticationFailed))
    }
}

pub async fn require_internal_service(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    match state
        .service_auth
        .verifier()
        .verify_headers(request.headers())
    {
        Ok(ctx) => {
            let context = RequestContext {
                request_id: ctx.request_id().clone(),
                correlation_id: ctx.correlation_id().clone(),
                causation_id: Some(CausationId::of_request(ctx.request_id())),
            };
            let span = tracing::info_span!("internal_request", request_id = %context.request_id,
                correlation_id = %context.correlation_id, caller = %ctx.authenticated_service(),
                method = %request.method(), route = route_pattern(&request));
            request.extensions_mut().insert(InternalCaller(ctx));
            let mut response = scoped(context.clone(), next.run(request).instrument(span)).await;
            if let Ok(value) = HeaderValue::from_str(context.request_id.as_str()) {
                response.headers_mut().insert(headers::REQUEST_ID, value);
            }
            response
        }
        Err(error) => {
            crate::metrics::inc(
                "messaging_internal_auth_failures_total",
                &[("reason", error.failure.as_str())],
            );
            tracing::warn!(
                reason = error.failure.as_str(),
                route = route_pattern(&request),
                "internal service authentication rejected"
            );
            let context = RequestContext::generate();
            let response = scoped(context.clone(), async {
                ApiError::code(ErrorCode::InternalAuthenticationFailed).into_response()
            })
            .await;
            with_request_id(response, &context)
        }
    }
}

/// The trusted context of an internal caller acting inside its signed delegated scope,
/// re-validated against Platform Access now (forged / stale chains and suspended
/// tenants, applications or environments are refused). Fail closed.
/// Like [`delegated_context`] but also accepts a valid chain whose authority is
/// SUSPENDED, returning whether it is at full authority. For restrictions (revocations)
/// that must apply precisely when the restricted authority is suspended.
pub async fn delegated_context_for_restriction(
    state: &AppState,
    caller: &InternalCaller,
) -> Result<(TrustedContext, bool), ApiError> {
    let Some(ctx) = caller.context().delegated_platform_context() else {
        return Err(ApiError::code(ErrorCode::DelegatedContextRequired));
    };
    use crate::identity::CustomerAuthority;
    match state.authority.validate_delegated(ctx).await {
        Ok(crate::identity::DelegatedScope::Valid {
            first_party,
            full_authority,
        }) => Ok((
            TrustedContext::delegated(ctx, first_party, full_authority),
            full_authority,
        )),
        Ok(crate::identity::DelegatedScope::Invalid) => {
            crate::metrics::inc("messaging_delegated_context_rejected_total", &[]);
            Err(ApiError::code(ErrorCode::DelegatedContextInvalid))
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            Err(ApiError::code(ErrorCode::ServiceUnavailable))
        }
    }
}

pub async fn delegated_context(
    state: &AppState,
    caller: &InternalCaller,
) -> Result<TrustedContext, ApiError> {
    let Some(ctx) = caller.context().delegated_platform_context() else {
        return Err(ApiError::code(ErrorCode::DelegatedContextRequired));
    };
    use crate::identity::CustomerAuthority;
    match state.authority.validate_delegated(ctx).await {
        Ok(crate::identity::DelegatedScope::Valid {
            first_party,
            full_authority,
        }) => {
            if !full_authority {
                return Err(ApiError::code(ErrorCode::AuthoritySuspended));
            }
            Ok(TrustedContext::delegated(ctx, first_party, full_authority))
        }
        Ok(crate::identity::DelegatedScope::Invalid) => {
            crate::metrics::inc("messaging_delegated_context_rejected_total", &[]);
            tracing::warn!(caller = %caller.context().authenticated_service(), "delegated platform context rejected");
            Err(ApiError::code(ErrorCode::DelegatedContextInvalid))
        }
        Err(e) => {
            tracing::warn!(error = %e, "identity authority unavailable");
            Err(ApiError::code(ErrorCode::ServiceUnavailable))
        }
    }
}

/// `/internal/v1/delegated/**`: a peer granted `messaging-application.delegate` acts as
/// the backend of its own (first-party) application environment on the standard
/// customer operations. The handlers see an ordinary [`CustomerCaller`] holding only
/// [`crate::identity::DELEGATED_SCOPES`]; acting principals are verified as usual.
pub async fn require_delegated_application(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(caller) = request.extensions().get::<InternalCaller>().cloned() else {
        return ApiError::code(ErrorCode::InternalAuthenticationFailed).into_response();
    };
    if let Err(e) = caller.require_permission(InternalPermission::MessagingApplicationDelegate) {
        return e.into_response();
    }
    let ctx = match delegated_context(&state, &caller).await {
        Ok(ctx) => ctx,
        Err(e) => return e.into_response(),
    };
    if !ctx.first_party {
        // Only first-party backends act through a peer service; external customers
        // present their own credential on /v1.
        return ApiError::code(ErrorCode::DelegatedContextInvalid).into_response();
    }
    // The end user the peer acts for travels SIGNED in its token (initiating principal).
    // A CF-Acting-Principal header may only repeat it; a different one is refused, so an
    // unsigned header can never re-target a signed request.
    if let Some(signed) = caller
        .context()
        .delegated_platform_context()
        .and_then(|d| d.initiating_principal())
    {
        match acting_header(request.headers()) {
            Ok(None) => {
                let value = HeaderValue::from_str(&signed.principal_id.hyphenated().to_string())
                    .expect("uuid is a valid header value");
                request.headers_mut().insert(ACTING_PRINCIPAL, value);
            }
            Ok(Some(p)) if p == signed.principal_id => {}
            Ok(Some(_)) => {
                return ApiError::reason(
                    ErrorCode::PrincipalNotAuthorized,
                    "ACTING_PRINCIPAL_MISMATCH",
                )
                .into_response()
            }
            Err(e) => return e.into_response(),
        }
    }
    let mutation = request.method() != Method::GET && request.method() != Method::HEAD;
    request.extensions_mut().insert(CustomerCaller(ctx));
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        state.wake_outbox();
    }
    response
}
