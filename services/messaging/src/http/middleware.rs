//! Request context and authentication (Part V C.8).
//!
//! | Routes | Authentication |
//! |---|---|
//! | `/health`, `/ready`, `/openapi.json` | none (no data) |
//! | `/v1/**` | `Authorization: Bearer <CF Meet API credential>` verified by Platform Access ([`CustomerCaller`]); `GET` may use the bounded, feed-invalidated cache, every other method authenticates afresh. `CF-Client-Token` is accepted only on the device-direct member operations ([`client_token_route`]) |
//! | `/v1/**` forwarded by the API Gateway (Part VI §51) | NO `Authorization`, instead an Ed25519 service token verified by Messaging's trust bundle, whose issuer holds `messaging-request.forward` and whose signed delegated context names the initiating credential; that credential is then authenticated LIVE by Platform Access (`POST /internal/v1/credentials/delegated-authority`, never cached). Both headers together, any other issuer, a missing grant or a missing credential id are refused like a bad credential |
//! | `/internal/v1/**` | an Ed25519 service token whose issuer Messaging's trust bundle grants the route's permission ([`InternalCaller`]) |
//! | operations | `GET /metrics` on a separate listener only |
//!
//! Both customer paths end in the same tail (`authenticated_customer`): the authority
//! chain Platform Access returned is the ONLY source of tenant, environment, scopes and
//! principal restriction; suspended (SAFETY_ONLY) chains, rejections, throttles and an
//! unreachable authority are answered identically.
//!
//! The ACTING principal ([`Acting`]) comes from `CF-Acting-Principal`, honoured only
//! inside the credential's principal restriction and when Platform Access resolves it
//! ACTIVE and messaging-capable in the credential's own environment — always fresh.
//!
//! ADR 0064: an OAuth credential (CF Meet Apps; `MESSAGING_APPS_ENABLED`) always acts as
//! its single restricted principal, on a closed MANAGED-only allowlist (`admit_oauth`).
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
    identity::{
        cache::Freshness, Authentication, AuthorityUnavailable, CustomerAuthority, PrincipalStatus,
        TrustedContext,
    },
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
    ("POST", "/v1/conversations/*/invitation/accept"),
    ("POST", "/v1/conversations/*/invitation/decline"),
    ("GET", "/v1/messaging/invitations"),
    ("GET", "/v1/conversations/*/messages"),
    ("POST", "/v1/conversations/*/messages"),
    ("GET", "/v1/conversations/*/messages/*"),
    ("PATCH", "/v1/conversations/*/messages/*"),
    ("DELETE", "/v1/conversations/*/messages/*"),
    // Gap audit S11 / S12: a member forwards a message and joins with an invite link.
    ("POST", "/v1/conversations/*/messages/*/forward"),
    ("POST", "/v1/messaging/invite-links/join"),
    ("GET", "/v1/conversations/*/messages/*/revisions"),
    ("GET", "/v1/conversations/*/messages/*/reactions"),
    ("GET", "/v1/conversations/*/messages/*/thread"),
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

/// ADR 0062 / build contract §5: the CLOSED allowlist of a partner session device (a
/// MANAGED-only session device exchanged from a CF Meet user token): conversations and
/// members (read), messages (list, get, send, edit, delete), reactions, the read cursor,
/// unread, events, attachments (create, finalize, get, download grants), search within a
/// conversation and reports. Never MLS, KeyPackages, Welcomes, backups, history
/// transfer, key transparency or device registration. Every route is also a
/// [`CLIENT_TOKEN_ROUTES`] member route; a PRIVATE_E2EE conversation is `NOT_FOUND`.
pub const PARTNER_SESSION_DEVICE_ROUTES: &[(&str, &str)] = &[
    ("GET", "/v1/conversations"),
    ("GET", "/v1/conversations/*"),
    ("GET", "/v1/conversations/*/members"),
    ("GET", "/v1/conversations/*/messages"),
    ("POST", "/v1/conversations/*/messages"),
    ("GET", "/v1/conversations/*/messages/*"),
    ("PATCH", "/v1/conversations/*/messages/*"),
    ("DELETE", "/v1/conversations/*/messages/*"),
    ("GET", "/v1/conversations/*/messages/*/reactions"),
    ("PUT", "/v1/conversations/*/messages/*/reactions/*"),
    ("DELETE", "/v1/conversations/*/messages/*/reactions/*"),
    ("PUT", "/v1/conversations/*/read-cursor"),
    ("GET", "/v1/conversations/*/unread"),
    ("GET", "/v1/conversations/*/events"),
    ("POST", "/v1/conversations/*/attachments"),
    ("GET", "/v1/conversations/*/attachments/*"),
    ("POST", "/v1/conversations/*/attachments/*/finalize"),
    ("POST", "/v1/conversations/*/attachments/*/download-grants"),
    ("GET", "/v1/conversations/*/search"),
    ("POST", "/v1/conversations/*/reports"),
];

/// Whether a partner session device may call `method path`.
pub fn partner_session_device_route(method: &Method, path: &str) -> bool {
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    PARTNER_SESSION_DEVICE_ROUTES.iter().any(|(m, pattern)| {
        let want: Vec<&str> = pattern.split('/').collect();
        *m == method.as_str()
            && want.len() == segments.len()
            && want
                .iter()
                .zip(&segments)
                .all(|(w, got)| (*w == "*" && !got.is_empty()) || w == got)
    })
}

/// Request extension set ONLY by the client-token path for a partner session device
/// (ADR 0062): handlers restrict what it sees to MANAGED conversations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartnerSessionDevice;

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
    request: Request,
    next: Next,
) -> Response {
    let context = RequestContext::generate();
    let span = tracing::info_span!("customer_request", request_id = %context.request_id,
        correlation_id = %context.correlation_id, method = %request.method(), route = route_pattern(&request));
    let response = scoped(
        context.clone(),
        async move {
            let authentication = if request
                .headers()
                .contains_key(headers::SERVICE_AUTHORIZATION)
            {
                let (method, path) = (request.method().clone(), received_path(&request));
                gateway_forwarded(&state, &method, &path, request.headers()).await
            } else {
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
                let freshness =
                    if request.method() == Method::GET || request.method() == Method::HEAD {
                        Freshness::BoundedStale
                    } else {
                        Freshness::Fresh
                    };
                state.authority.authenticate_with(&token, freshness).await
            };
            authenticated_customer(&state, authentication, request, next).await
        }
        .instrument(span),
    )
    .await;
    with_request_id(response, &context)
}

/// Part VI §51: a `/v1` request the public API Gateway authenticated and forwarded. The
/// gateway strips `Authorization` and signs a fresh service token instead; it is accepted
/// only when Messaging's trust bundle grants its issuer `messaging-request.forward` and its
/// signed delegated context names the initiating credential. That credential is then
/// authenticated LIVE by Platform Access — the token itself is never authority. Every
/// refusal is a [`Authentication::Rejected`] (the normalized 401 of a bad credential);
/// reasons are fixed vocabulary, token values are never logged.
async fn gateway_forwarded(
    state: &AppState,
    method: &axum::http::Method,
    path: &str,
    headers: &HeaderMap,
) -> Result<Authentication, AuthorityUnavailable> {
    let refused = |reason: &'static str| {
        crate::metrics::inc(
            "messaging_gateway_forward_refused_total",
            &[("reason", reason)],
        );
        Ok(Authentication::Rejected {
            reason: format!("gateway_forward_{reason}"),
            throttle: None,
        })
    };
    // A service token never accompanies a customer credential.
    if headers.contains_key(header::AUTHORIZATION) {
        return refused("customer_authorization_present");
    }
    // SEC-012: bound to this request (method + received path); single-use on mutating
    // methods.
    let verified = state
        .service_auth
        .verifier()
        .verify_request(method, path, headers)
        .await;
    let caller = match verified {
        Ok(caller) => caller,
        Err(error) => return refused(error.failure.as_str()),
    };
    if !caller.has_permission(InternalPermission::MessagingRequestForward) {
        tracing::warn!(caller = %caller.authenticated_service(), "service token without messaging-request.forward on a customer route");
        return refused("permission_denied");
    }
    let Some(delegated) = caller
        .delegated_platform_context()
        .filter(|d| d.initiating_credential_id().is_some())
    else {
        return refused("credential_id_missing");
    };
    let outcome = state
        .authority
        .authenticate_forwarded(delegated, caller.correlation_id())
        .await;
    crate::metrics::inc(
        "messaging_gateway_forward_total",
        &[(
            "outcome",
            match &outcome {
                Ok(Authentication::Authenticated(_)) => "authenticated",
                Ok(Authentication::Rejected { .. }) => "rejected",
                Err(_) => "authority_unavailable",
            },
        )],
    );
    outcome
}

/// The common tail of every customer authentication: only a FULL authority chain becomes
/// the request's [`CustomerCaller`]; a suspended scope, a rejection (throttled or not) and
/// an unreachable authority are answered the same way whichever path authenticated.
async fn authenticated_customer(
    state: &AppState,
    authentication: Result<Authentication, AuthorityUnavailable>,
    mut request: Request,
    next: Next,
) -> Response {
    match authentication {
        Ok(Authentication::Authenticated(ctx)) if ctx.full_authority => {
            // ADR 0064: a CF Meet App's OAuth credential acts only as its restricted
            // principal, on the closed MANAGED-only allowlist (refused with the flag off).
            if ctx.oauth.is_some() {
                if let Err(e) = admit_oauth(state, &ctx, &mut request).await {
                    return e.into_response();
                }
            }
            // ADR 0062 / build contract §5: with partner session devices on, an EXTERNAL
            // environment gets the CFMEET_PLATFORM_PARTNER baseline (once; never
            // replacing another baseline; its own application policy still wins).
            if !ctx.first_party && state.partner.settings.session_devices_enabled {
                state
                    .partner
                    .ensure_baseline(&state.db, &ctx.scope, state.now())
                    .await;
            }
            let mutation = request.method() != Method::GET && request.method() != Method::HEAD;
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
    // ADR 0062: a partner session device reaches its closed MANAGED-only allowlist.
    let partner = if session.is_session_device() && state.partner.settings.session_devices_enabled {
        match crate::realtime::session_devices::active_device(
            &state.db,
            session.device_id,
            session.principal_id,
            session.environment_id,
            state.now(),
        )
        .await
        {
            Ok(d) => d.is_some_and(|d| d.is_partner()),
            Err(e) => return e.into_response(),
        }
    } else {
        false
    };
    if partner {
        if !partner_session_device_route(request.method(), request.uri().path()) {
            crate::metrics::inc(
                "messaging_client_token_rest_total",
                &[("outcome", "partner_route_refused")],
            );
            return ApiError::reason(
                ErrorCode::ClientTokensNotEnabled,
                "PARTNER_ROUTE_NOT_ALLOWED",
            )
            .into_response();
        }
        if let Err(e) =
            require_managed_target(state, session.environment_id, request.uri().path()).await
        {
            if e.error_code() == ErrorCode::NotFound {
                crate::metrics::inc(
                    "messaging_client_token_rest_total",
                    &[("outcome", "partner_e2ee_refused")],
                );
            }
            return e.into_response();
        }
        request.extensions_mut().insert(PartnerSessionDevice);
    }
    // Section 13: a session device is accepted only by the realtime gateway; it never
    // reaches a device-direct REST operation (KeyPackages, Welcomes, MLS commits,
    // ciphertext, push registration, ...).
    if session.is_session_device() && !partner {
        crate::metrics::inc(
            "messaging_client_token_rest_total",
            &[("outcome", "session_device_refused")],
        );
        return ApiError::reason(
            ErrorCode::ClientTokensNotEnabled,
            "SESSION_DEVICE_REALTIME_ONLY",
        )
        .into_response();
    }
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

/// ADR 0064 (integrate contract §7): an OAuth credential (`credentialKind`
/// `OAUTH_ACCESS` / `OAUTH_BOT`). With `MESSAGING_APPS_ENABLED` it reaches only
/// [`crate::apps::oauth_route`], acts only as the ONE principal its restriction names
/// (`CF-Acting-Principal` is set to it; a different one is refused) and sees MANAGED
/// conversations only (a PRIVATE_E2EE target is `NOT_FOUND`). With the flag off it is
/// refused: it never acts like an ordinary credential.
async fn admit_oauth(
    state: &AppState,
    ctx: &TrustedContext,
    request: &mut Request,
) -> Result<(), ApiError> {
    let refused = |outcome: &'static str, e: ApiError| {
        crate::metrics::inc("messaging_oauth_requests_total", &[("outcome", outcome)]);
        tracing::info!(outcome, "OAuth credential refused");
        Err(e)
    };
    if !state.apps.enabled {
        return refused(
            "not_enabled",
            ApiError::reason(ErrorCode::InsufficientScope, "OAUTH_NOT_ENABLED"),
        );
    }
    if !crate::apps::oauth_route(request.method(), request.uri().path()) {
        return refused(
            "route_not_allowed",
            ApiError::reason(ErrorCode::InsufficientScope, "OAUTH_ROUTE_NOT_ALLOWED"),
        );
    }
    let Some(principal) = ctx.oauth_principal() else {
        return refused(
            "restriction_invalid",
            ApiError::reason(ErrorCode::PrincipalNotAuthorized, "CREDENTIAL_RESTRICTION"),
        );
    };
    match acting_header(request.headers()) {
        Ok(None) => {
            let value = HeaderValue::from_str(&principal.hyphenated().to_string())
                .expect("uuid is a valid header value");
            request.headers_mut().insert(ACTING_PRINCIPAL, value);
        }
        Ok(Some(p)) if p == principal => {}
        Ok(Some(_)) => {
            return refused(
                "acting_mismatch",
                ApiError::reason(
                    ErrorCode::PrincipalNotAuthorized,
                    "ACTING_PRINCIPAL_MISMATCH",
                ),
            )
        }
        Err(e) => return Err(e),
    }
    if let Err(e) =
        require_managed_target(state, ctx.scope.environment_id, request.uri().path()).await
    {
        if e.error_code() == ErrorCode::NotFound {
            crate::metrics::inc(
                "messaging_oauth_requests_total",
                &[("outcome", "e2ee_refused")],
            );
        }
        return Err(e);
    }
    crate::metrics::inc("messaging_oauth_requests_total", &[("outcome", "accepted")]);
    Ok(())
}

/// A partner session device and an OAuth credential are MANAGED-only: a conversation
/// route whose conversation is PRIVATE_E2EE answers `NOT_FOUND` (indistinguishable from
/// a foreign conversation).
async fn require_managed_target(
    state: &AppState,
    environment_id: Uuid,
    path: &str,
) -> Result<(), ApiError> {
    let Some(id) = path
        .strip_prefix("/v1/conversations/")
        .and_then(|rest| rest.split('/').next())
    else {
        return Ok(());
    };
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(()); // the handler refuses a malformed id
    };
    let e2ee = crate::db::scalar_i64(
        &state.db,
        "SELECT count(*) FROM messaging_conversations WHERE id = $1 AND environment_id = $2 \
           AND security_profile <> 'MANAGED'",
        vec![id.into(), environment_id.into()],
    )
    .await?;
    if e2ee > 0 {
        return Err(ApiError::code(ErrorCode::NotFound));
    }
    Ok(())
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

/// The request-target path as received (before any `Router::nest` prefix stripping),
/// which is what a signer binds in `htu`.
pub(crate) fn received_path(request: &Request) -> String {
    request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|u| u.0.path().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned())
}

pub async fn require_internal_service(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    // SEC-012: bound to this request (method + received path); single-use on mutating
    // methods.
    let (method, path) = (request.method().clone(), received_path(&request));
    let verified = state
        .service_auth
        .verifier()
        .verify_request(&method, &path, request.headers())
        .await;
    match verified {
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

/// Request extension set ONLY by [`require_delegated_application`]: the request is a
/// first-party backend acting through a peer service granted
/// `messaging-application.delegate` (the delegated mirror). Never present on `/v1`
/// requests — customer credentials, gateway-forwarded credentials and client tokens —
/// so caller-controlled options reserved for mirrored / migrated writes (e.g. a message's
/// `notify: false`, S-4) can tell the two apart. Handlers read it with
/// `Option<Extension<DelegatedApplication>>`; a request cannot set an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelegatedApplication;

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
    request.extensions_mut().insert(DelegatedApplication);
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        state.wake_outbox();
    }
    response
}

/// Request extension set ONLY by [`require_organization_moderation`]: the organization
/// moderator (their principal in the organization's own Messaging scope) a moderation
/// request was relayed for. Recorded on moderation actions and audit entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrganizationModerator(pub Uuid);

/// The single scope an organization moderation request holds.
pub const ORGANIZATION_MODERATION_SCOPES: &[&str] = &["moderation.manage"];

/// `/internal/v1/organization-moderation/**` (gap audit G6): the Meeting Service, granted
/// `messaging-organization.govern`, relays a request of an organization moderator — a
/// member whose organization grants `messaging.organization.moderate`, decided by the
/// Organizations Service on every request before anything reaches Messaging — into the
/// organization's OWN first-party scope. The handlers are the ordinary moderation
/// handlers (one moderation path) and see a [`CustomerCaller`] holding only
/// `moderation.manage`. The moderator must travel SIGNED as the initiating principal and
/// be ACTIVE in that scope; it is never an acting member (no CF-Acting-Principal), and is
/// recorded as [`OrganizationModerator`]. The organization's scope bounds what is visible:
/// the reports and conversations of that organization only.
pub async fn require_organization_moderation(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(caller) = request.extensions().get::<InternalCaller>().cloned() else {
        return ApiError::code(ErrorCode::InternalAuthenticationFailed).into_response();
    };
    if let Err(e) = caller.require_permission(InternalPermission::MessagingOrganizationGovern) {
        return e.into_response();
    }
    let mut ctx = match delegated_context(&state, &caller).await {
        Ok(ctx) => ctx,
        Err(e) => return e.into_response(),
    };
    if !ctx.first_party {
        return ApiError::code(ErrorCode::DelegatedContextInvalid).into_response();
    }
    let Some(signed) = caller
        .context()
        .delegated_platform_context()
        .and_then(|d| d.initiating_principal())
    else {
        return ApiError::reason(ErrorCode::PrincipalNotAuthorized, "MODERATOR_REQUIRED")
            .into_response();
    };
    match acting_header(request.headers()) {
        Ok(None) => {}
        Ok(Some(_)) => {
            return ApiError::reason(ErrorCode::PrincipalNotAuthorized, "ADMINISTRATION_ONLY")
                .into_response()
        }
        Err(e) => return e.into_response(),
    }
    let moderator = match verify_principal(&state, &ctx, signed.principal_id).await {
        Ok(a) => a.principal_id,
        Err(e) => return e.into_response(),
    };
    ctx.scopes = ORGANIZATION_MODERATION_SCOPES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mutation = request.method() != Method::GET && request.method() != Method::HEAD;
    request.extensions_mut().insert(CustomerCaller(ctx));
    request
        .extensions_mut()
        .insert(OrganizationModerator(moderator));
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        state.wake_outbox();
    }
    response
}
