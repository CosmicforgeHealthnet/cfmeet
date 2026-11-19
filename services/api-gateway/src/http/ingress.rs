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
//!
//!    * USER_TOKEN (`Bearer cfut_…`, ADR 0062, `API_GATEWAY_USER_TOKENS_ENABLED`): the
//!      signature and expiry are pre-checked statelessly with Platform Access's published
//!      keys ([`crate::user_tokens`]; refused 401 before forwarding, 503 without usable
//!      keys), then `Authorization` is transported unchanged — the owning service
//!      verifies the token live.
//!
//!    * OAUTH_ACCESS_TOKEN (`Bearer cfoa_…` / `Bearer cfob_…`, ADR 0064,
//!      `API_GATEWAY_OAUTH_ENABLED`): a CF Meet Apps token, authenticated through Platform
//!      Access exactly like a platform credential (same refusals, scope pre-check and
//!      quotas). Platform Access must report the matching `credentialKind`
//!      (`OAUTH_ACCESS` for `cfoa_`, `OAUTH_BOT` for `cfob_`) and a principal restriction
//!      of exactly one principal, otherwise 401. The upstream receives the signed
//!      delegated context with `initiating_credential_id` = the token's credential id and
//!      `initiating_principal` = the restricted principal (`NATIVE_CF_USER` for access,
//!      `BOT_OR_SERVICE` for bot tokens) — never the token. A `cfor_` refresh token is
//!      refused 401 everywhere; an OAuth token on a route that does not list
//!      OAUTH_ACCESS_TOKEN is refused `403 CREDENTIAL_NOT_ACCEPTED` (never transported),
//!      except on a `forwardAuthorization` route (the OAuth endpoints of the Developer
//!      Console, ANONYMOUS): there `Authorization` — an OAuth access / bot bearer token
//!      (userinfo) or `Basic` client credentials (`client_secret_basic` on token /
//!      revoke) — is transported unchanged and the Developer Console verifies it with
//!      Platform Access. Nothing else is transported there (a platform credential, a
//!      refresh token or any other bearer value is refused 401).
//!
//!      Quotas: the authority chain of an OAuth token is the INSTALL TARGET (the user's
//!      or organization's FIRST_PARTY environment and the installation service account),
//!      so the platform request quota is that environment's (the app's own developer
//!      environment is not charged), and the API_OPERATION usage is recorded for that
//!      environment with the token's credential id and the installation service account
//!      as the caller dimensions.
//!
//!    Anonymous requests to the Developer Platform (OAuth token / revoke / metadata, the
//!    app directory) carry a gateway service token WITHOUT a delegated context or a
//!    session — the Developer Platform's public API is reachable only through the
//!    gateway; anonymous requests to other services are transported as before.
//!
//!    End-user / device traffic (every mode above except PLATFORM_CREDENTIAL,
//!    OAUTH_ACCESS_TOKEN and anonymous) is held to a per-token ceiling when
//!    `API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE` is set (Section 15, PLT-012): the bucket
//!    is a digest of the presented token (never the token), counted in the platform
//!    quota store; over it the request is refused `429 RATE_LIMITED` with `Retry-After`
//!    and the `x-cf-quota-window` / `x-cf-quota-level: GATEWAY_TOKEN` headers. Anonymous
//!    requests carry nothing to key on (client addresses are never taken from spoofable
//!    forwarding headers) and stay with the services and the edge.
//! 5. Bounded body (any media type the route's contract declares — JSON, or
//!    `application/x-www-form-urlencoded` for the OAuth endpoints — forwarded verbatim with
//!    its `Content-Type`), per-route deadline, safe upstream errors; the owning service keeps
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
use cf_service_auth::{services, OutboundServiceRequest};
use futures_util::TryStreamExt;
use tracing::Instrument;

use super::error::{ApiError, ErrorCode};
use crate::{
    context::{scoped, RequestContext},
    headers::{
        presents_service_token, safe_value, stripped_authority_headers, FORWARDED_REQUEST_HEADERS,
        RELAYED_RESPONSE_HEADERS,
    },
    identity::{AuthenticatedCustomer, Authentication, CredentialKind},
    quota::{token_bucket, Decision, TokenDecision, Window},
    routing::{AuthMode, Lookup, Protocol, RouteMatch},
    state::AppState,
    user_tokens::{unix_now, UserTokenError, USER_TOKEN_PREFIX},
};

pub const USER_SESSION_HEADER: &str = "x-cf-user-session";
pub const CREDENTIAL_PREFIX: &str = "cfmk_";
pub const SIGNALING_CREDENTIAL_PREFIX: &str = "cfsig_";
/// CF Meet Apps OAuth tokens (ADR 0064).
pub const OAUTH_ACCESS_PREFIX: &str = "cfoa_";
pub const OAUTH_BOT_PREFIX: &str = "cfob_";
pub const OAUTH_REFRESH_PREFIX: &str = "cfor_";

/// The OAuth credential kind a presented token claims by its prefix (`None`: not an
/// OAuth access or bot token).
pub fn oauth_token_kind(token: &str) -> Option<CredentialKind> {
    if token.starts_with(OAUTH_ACCESS_PREFIX) {
        Some(CredentialKind::OauthAccess)
    } else if token.starts_with(OAUTH_BOT_PREFIX) {
        Some(CredentialKind::OauthBot)
    } else {
        None
    }
}

fn is_oauth_token(token: &str) -> bool {
    oauth_token_kind(token).is_some() || token.starts_with(OAUTH_REFRESH_PREFIX)
}

/// Response extension: the environment of the device token a request was authenticated
/// with, for the partner-website CORS decision ([`crate::http`]). Set only for device
/// authentication modes, never for platform credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartnerEnvironment(pub uuid::Uuid);

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

/// Chooses the authentication mode (see the module documentation). `user_tokens`:
/// `API_GATEWAY_USER_TOKENS_ENABLED` (off: a `cfut_` token is treated exactly as before);
/// `oauth`: `API_GATEWAY_OAUTH_ENABLED` (off: `cfoa_` / `cfob_` / `cfor_` tokens are
/// treated exactly as before).
pub fn select_mode(
    route: &RouteMatch<'_>,
    auth: Option<&Presented>,
    headers: &HeaderMap,
    user_tokens: bool,
    oauth: bool,
) -> Result<AuthMode, ApiError> {
    let d = route.definition;
    match auth {
        Some(p) if oauth && is_oauth_token(&p.token) => {
            if !p.scheme.eq_ignore_ascii_case("bearer") || oauth_token_kind(&p.token).is_none() {
                // A refresh token is never a request credential (and an OAuth token
                // travels only as a bearer token).
                Err(ApiError::Code(ErrorCode::AuthenticationFailed))
            } else if d.allows(AuthMode::OauthAccessToken) {
                Ok(AuthMode::OauthAccessToken)
            } else if d.forward_authorization && d.allows(AuthMode::Anonymous) {
                // Dedicated pass-through (OAuth userinfo): the Developer Console
                // introspects the token itself.
                Ok(AuthMode::Anonymous)
            } else {
                // Never transported to a service that does not take it.
                Err(ApiError::Code(ErrorCode::CredentialNotAccepted))
            }
        }
        Some(p)
            if user_tokens
                && p.scheme.eq_ignore_ascii_case("bearer")
                && p.token.starts_with(USER_TOKEN_PREFIX)
                && d.allows(AuthMode::UserToken) =>
        {
            Ok(AuthMode::UserToken)
        }
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
            } else if d.allows(AuthMode::SignalingCredential)
                && p.token.starts_with(SIGNALING_CREDENTIAL_PREFIX)
            {
                Ok(AuthMode::SignalingCredential)
            } else if d.allows(AuthMode::MeetingSession) {
                Ok(AuthMode::MeetingSession)
            } else {
                Err(ApiError::Code(ErrorCode::AuthenticationFailed))
            }
        }
        // OAuth client authentication (`client_secret_basic`) on the token / revoke
        // endpoints: transported to the Developer Console, which verifies the client
        // secret with Platform Access.
        Some(p)
            if oauth
                && p.scheme.eq_ignore_ascii_case("basic")
                && d.forward_authorization
                && d.allows(AuthMode::Anonymous) =>
        {
            Ok(AuthMode::Anonymous)
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
                // (A user token always travels in `Authorization`.)
                if let Some(m) = d
                    .auth
                    .iter()
                    .copied()
                    .find(|m| m.is_passthrough() && *m != AuthMode::UserToken)
                {
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
    let (parts, body) = request.into_parts();
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
    let mode = match select_mode(
        &route,
        auth.as_ref(),
        &parts.headers,
        state.flags.user_tokens,
        state.flags.oauth,
    ) {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let partner_environment = if mode == AuthMode::UserToken {
        match verify_user_token(&state, auth.as_ref(), &route_id).await {
            Ok(environment) => Some(environment),
            Err(e) => return e.into_response(),
        }
    } else if state.origin_index().is_some() && parts.headers.contains_key(header::ORIGIN) {
        unverified_environment(mode, auth.as_ref())
    } else {
        None
    };
    let mut response = dispatch(&state, &context, &route, parts, body, auth, mode).await;
    if let Some(environment) = partner_environment {
        response
            .extensions_mut()
            .insert(PartnerEnvironment(environment));
    }
    response
}

/// USER_TOKEN: the stateless pre-check (the token is never logged).
async fn verify_user_token(
    state: &AppState,
    auth: Option<&Presented>,
    route_id: &str,
) -> Result<uuid::Uuid, ApiError> {
    let Some(verifier) = state.user_token_verifier() else {
        tracing::warn!(route = route_id, "user tokens enabled without a verifier");
        return Err(ApiError::Code(ErrorCode::ServiceUnavailable));
    };
    let token = auth.map(|a| a.token.as_str()).unwrap_or_default();
    match verifier.verify(token, unix_now()).await {
        Ok(verified) => Ok(verified.environment_id),
        Err(UserTokenError::KeysUnavailable) => {
            tracing::warn!(
                route = route_id,
                "user token refused: no usable signing keys (fail closed)"
            );
            Err(ApiError::Code(ErrorCode::ServiceUnavailable))
        }
        Err(e) => {
            tracing::info!(route = route_id, reason = e.as_str(), "user token refused");
            Err(ApiError::Code(ErrorCode::AuthenticationFailed))
        }
    }
}

/// The environment claimed by a client token (`eid`) or a signaling credential
/// (`cfsig_…`, `env`), read WITHOUT verifying the signature: those tokens are signed
/// with keys only their owning service holds.
///
/// Safe for its only use — choosing whether a partner origin may READ the response
/// (`Access-Control-Allow-Origin`): the request is forwarded exactly as it would be
/// without CORS, and the owning service verifies the token and rejects a forged or
/// altered claim (401) before doing anything. A forged claim can therefore only expose
/// that error to the origin of the environment the forger named, never data of the
/// environment; and CORS is not an authentication boundary (a token holder can call the
/// API from a server without it). It is never used for authorization.
pub fn unverified_environment(mode: AuthMode, auth: Option<&Presented>) -> Option<uuid::Uuid> {
    let token = auth?.token.as_str();
    let (jws, claim) = match mode {
        AuthMode::ClientToken => (token, "eid"),
        AuthMode::SignalingCredential => (token.strip_prefix(SIGNALING_CREDENTIAL_PREFIX)?, "env"),
        _ => return None,
    };
    use base64::Engine;
    let payload = jws.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    uuid::Uuid::parse_str(claims.get(claim)?.as_str()?).ok()
}

/// Everything after authentication-mode selection (and the user-token pre-check).
async fn dispatch(
    state: &AppState,
    context: &RequestContext,
    route: &RouteMatch<'_>,
    mut parts: Parts,
    body: Body,
    auth: Option<Presented>,
    mode: AuthMode,
) -> Response {
    let route_id = route.definition.id.clone();
    if let Some(refused) = token_ceiling(state, mode, auth.as_ref(), &parts.headers).await {
        tracing::info!(route = %route_id, mode = mode.as_str(), "per-token request ceiling exceeded");
        return refused;
    }
    let upgrade = is_websocket_upgrade(&parts.headers);
    if route.definition.protocol == Protocol::WebSocket {
        if !upgrade || parts.method != Method::GET {
            return ApiError::Code(ErrorCode::ValidationError).into_response();
        }
        if !mode.is_passthrough() {
            return ApiError::Code(ErrorCode::ProtocolNotEnabled).into_response();
        }
        return crate::ws::proxy(state, route, &mut parts, auth.as_ref(), mode).await;
    }
    if upgrade {
        return ApiError::Code(ErrorCode::ValidationError).into_response();
    }
    let forward = match mode {
        AuthMode::PlatformCredential | AuthMode::OauthAccessToken => {
            let token = auth.as_ref().map(|a| a.token.clone()).unwrap_or_default();
            let customer = match authenticate(state, &token, context, &route_id).await {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };
            if let Err(e) = check_credential_kind(mode, &token, &customer, &route_id) {
                return e.into_response();
            }
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
            .check_for(
                customer.environment_id,
                customer.environment_kind,
                Some(crate::quota::Subject {
                    service_account_id: customer.service_account_id,
                    credential_id: customer.credential_id,
                }),
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
    let credential_environment = match &forward {
        Forward::Credential(customer) => Some((
            customer.environment_id,
            crate::quota::Subject {
                service_account_id: customer.service_account_id,
                credential_id: customer.credential_id,
            },
            customer.environment_kind,
        )),
        _ => None,
    };
    let mut response = forward_rest(state, context, route, &parts, bytes, forward).await;
    // Normalized API_OPERATION usage after final response classification (credential
    // requests: the application environment is known).
    if let (Some((environment, subject, kind)), Some(engine)) =
        (credential_environment, &state.quota)
    {
        let definition = route.definition;
        let operation = definition
            .id
            .split_once('.')
            .map(|(_, o)| o)
            .unwrap_or(&definition.id);
        engine.record_caller_operation(
            environment,
            Some(subject),
            Some(kind),
            &definition.domain,
            operation,
            response.status().as_u16(),
        );
    }
    for (k, v) in quota_headers {
        if let Ok(v) = HeaderValue::from_str(&v) {
            response.headers_mut().insert(k, v);
        }
    }
    response
}

/// The authenticated credential must be of the presented kind: an API key (no OAuth
/// kind) for PLATFORM_CREDENTIAL; for OAUTH_ACCESS_TOKEN the OAuth kind of the token's
/// prefix (`cfoa_` access, `cfob_` bot) restricted to exactly one principal (it becomes
/// the forwarded initiating principal). Anything else is refused 401.
fn check_credential_kind(
    mode: AuthMode,
    token: &str,
    customer: &AuthenticatedCustomer,
    route_id: &str,
) -> Result<(), ApiError> {
    let ok = match (mode, customer.oauth) {
        (AuthMode::PlatformCredential, None) => true,
        (AuthMode::OauthAccessToken, Some(oauth)) => {
            if Some(oauth.kind) != oauth_token_kind(token) {
                false
            } else if customer.oauth_principal().is_none() {
                tracing::warn!(
                    route = route_id,
                    app = %oauth.app_id,
                    grant = %oauth.grant_id,
                    "oauth credential refused: the principal restriction is not exactly one principal"
                );
                return Err(ApiError::Code(ErrorCode::AuthenticationFailed));
            } else {
                tracing::info!(
                    route = route_id,
                    kind = oauth.kind.as_str(),
                    app = %oauth.app_id,
                    grant = %oauth.grant_id,
                    "oauth credential authenticated"
                );
                true
            }
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        tracing::warn!(
            route = route_id,
            mode = mode.as_str(),
            "authentication refused: credential kind does not match the presented token"
        );
        Err(ApiError::Code(ErrorCode::AuthenticationFailed))
    }
}

/// The end-user / device token a passthrough or session request is counted under
/// (`None`: platform credentials and OAuth tokens — metered by the platform quotas — and
/// requests that present no token).
pub fn rate_limited_token<'a>(
    mode: AuthMode,
    auth: Option<&'a Presented>,
    headers: &'a HeaderMap,
) -> Option<&'a str> {
    match mode {
        AuthMode::PlatformCredential | AuthMode::OauthAccessToken | AuthMode::Anonymous => None,
        AuthMode::ParticipantToken => headers
            .get("x-participant-token")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
            .or(auth.map(|a| a.token.as_str())),
        _ => auth.map(|a| a.token.as_str()),
    }
}

/// PLT-012: the per-token ceiling (`None`: allowed, or off).
async fn token_ceiling(
    state: &AppState,
    mode: AuthMode,
    auth: Option<&Presented>,
    headers: &HeaderMap,
) -> Option<Response> {
    let per_minute = state.token_rate_limit_per_minute;
    let engine = state.quota.as_ref().filter(|_| per_minute > 0)?;
    let token = rate_limited_token(mode, auth, headers)?;
    match engine
        .check_token(mode.as_str(), &token_bucket(token), per_minute)
        .await
    {
        TokenDecision::Allowed { .. } => None,
        TokenDecision::Exceeded {
            retry_after_seconds,
        } => {
            let mut r = ApiError::RateLimited {
                retry_after_seconds,
            }
            .into_response();
            r.headers_mut().insert(
                "x-cf-quota-window",
                HeaderValue::from_static("requests-per-minute"),
            );
            r.headers_mut().insert(
                "x-cf-quota-level",
                HeaderValue::from_static("GATEWAY_TOKEN"),
            );
            Some(r)
        }
    }
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
        Some(
            AuthMode::ClientToken
            | AuthMode::SignalingCredential
            | AuthMode::MeetingSession
            | AuthMode::UserToken,
        ) => {
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
        // The dedicated OAuth pass-through ([`RouteDefinition::forward_authorization`]):
        // only an OAuth access / bot bearer token or OAuth client credentials (Basic),
        // only on the marked route.
        Some(AuthMode::Anonymous) if route.definition.forward_authorization => {
            if let Some(a) = auth.filter(|a| {
                (a.scheme.eq_ignore_ascii_case("bearer") && oauth_token_kind(&a.token).is_some())
                    || a.scheme.eq_ignore_ascii_case("basic")
            }) {
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
    // SEC-012: a service token is bound (when emission is on) to the method and the path
    // exactly as the upstream receives them: the parsed upstream URL's path.
    // (An unparsable URL fails below exactly as before; binding then refuses to sign.)
    let upstream_path = reqwest::Url::parse(&url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    let upstream_path = upstream_path.as_str();
    match &forward {
        Forward::Credential(customer) => {
            let hop =
                OutboundServiceRequest::continuing(route.upstream.clone(), &context.correlation_id)
                    .with_delegated_context(customer.delegated());
            match state
                .service_auth
                .signer()
                .sign_request(&hop, &parts.method, upstream_path)
            {
                Ok(signed) => signed.apply_to(&mut headers),
                Err(_) => return ApiError::Code(ErrorCode::InternalError).into_response(),
            }
        }
        Forward::Session(token) => {
            let hop =
                OutboundServiceRequest::continuing(route.upstream.clone(), &context.correlation_id);
            match (
                state
                    .service_auth
                    .signer()
                    .sign_request(&hop, &parts.method, upstream_path),
                HeaderValue::from_str(token),
            ) {
                (Ok(signed), Ok(session)) => {
                    signed.apply_to(&mut headers);
                    headers.insert(HeaderName::from_static(USER_SESSION_HEADER), session);
                }
                _ => return ApiError::Code(ErrorCode::AuthenticationFailed).into_response(),
            }
        }
        // The Developer Platform's public API is reachable only through the gateway: an
        // anonymous request carries a gateway token with no delegated context and no
        // session (the Developer Platform decides what an anonymous caller may do).
        Forward::Passthrough(AuthMode::Anonymous)
            if route.upstream.as_str() == services::DEVELOPER_PLATFORM =>
        {
            let hop =
                OutboundServiceRequest::continuing(route.upstream.clone(), &context.correlation_id);
            match state
                .service_auth
                .signer()
                .sign_request(&hop, &parts.method, upstream_path)
            {
                Ok(signed) => signed.apply_to(&mut headers),
                Err(_) => return ApiError::Code(ErrorCode::InternalError).into_response(),
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
