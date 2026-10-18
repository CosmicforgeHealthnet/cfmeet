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
    ClientToken,
    Invalid,
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
        Presented::ClientToken
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
                Presented::ClientToken => {
                    return ApiError::code(ErrorCode::ClientTokensNotEnabled).into_response()
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
                    request.extensions_mut().insert(CustomerCaller(*ctx));
                    next.run(request).await
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
