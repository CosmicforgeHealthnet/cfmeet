//! Request context and authentication.
//!
//! * Public probes (`/health`, `/ready`, `/openapi.json`) and provider callbacks get a
//!   fresh request context (callbacks are authenticated by the provider adapter).
//! * `/v1/**` (customer): `Authorization: Bearer <credential>` verified by Platform
//!   Access on every request ([`CustomerCaller`]); the tenant / application / environment
//!   come ONLY from the authority chain. Every failure is the same 401; throttling is a
//!   429; the authority unreachable is a 503 (fail closed). A service token is never a
//!   customer credential.
//! * `/internal/v1/**` (services): a valid `X-CF-Service-Authorization` service token
//!   whose issuer the SMS Gateway's trust bundle grants the route's permission
//!   ([`InternalCaller`]); the request continues the signed correlation id and records the
//!   upstream request as its cause.
//!
//! Spans never carry content or numbers.
//!
//! Spans carry the matched ROUTE PATTERN, never the path; credentials and tokens are never
//! logged.

use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{header, request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers, InternalPermission, InternalServiceContext};
use tracing::Instrument;

use super::error::{ApiError, ErrorCode};
use crate::{
    domain::{scoped, CausationId, RequestContext},
    identity::{Authentication, TrustedContext},
    state::AppState,
};

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
    pub fn require_scope(&self, scope: &str) -> Result<&TrustedContext, ApiError> {
        if self.0.has_scope(scope) {
            Ok(&self.0)
        } else {
            crate::metrics::inc("sms_scope_denied_total", &[]);
            tracing::info!(scope, "scope check denied");
            Err(ApiError::Code(ErrorCode::InsufficientScope))
        }
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
            .ok_or(ApiError::Code(ErrorCode::AuthenticationFailed))
    }
}

/// `Authorization: Bearer <value>` (exactly one header, bounded).
fn bearer(request: &Request) -> Option<String> {
    let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > 1_024 {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty())
        .then(|| token.trim().to_string())
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
                return ApiError::Code(ErrorCode::AuthenticationFailed).into_response();
            }
            let Some(token) = bearer(&request) else {
                return ApiError::Code(ErrorCode::AuthenticationFailed).into_response();
            };
            match state.authority.authenticate(&token).await {
                Ok(Authentication::Authenticated(ctx)) if ctx.full_authority => {
                    request.extensions_mut().insert(CustomerCaller(*ctx));
                    next.run(request).await
                }
                Ok(Authentication::Authenticated(_)) => {
                    tracing::info!("customer authentication refused (suspended scope)");
                    ApiError::Code(ErrorCode::AuthenticationFailed).into_response()
                }
                Ok(Authentication::Rejected { reason, throttle }) => {
                    crate::metrics::inc("sms_customer_auth_rejected_total", &[]);
                    tracing::info!(%reason, "customer authentication rejected");
                    match throttle {
                        Some(t) => ApiError::RateLimited {
                            retry_after_seconds: t.retry_after_seconds,
                            reason: None,
                        }
                        .into_response(),
                        None => ApiError::Code(ErrorCode::AuthenticationFailed).into_response(),
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "identity authority unavailable");
                    ApiError::Code(ErrorCode::ServiceUnavailable).into_response()
                }
            }
        }
        .instrument(span),
    )
    .await;
    with_request_id(response, &context)
}

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct InternalCaller(InternalServiceContext);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// Fails with INTERNAL_PERMISSION_DENIED unless the SMS Gateway's trust bundle grants
    /// `permission` to the caller.
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.0.require_permission(permission).map_err(|denied| {
            crate::metrics::inc("sms_permission_denied_total", &[("permission", denied.permission.as_str())]);
            tracing::warn!(caller = %denied.service, permission = denied.permission.as_str(), "internal permission denied");
            ApiError::Code(ErrorCode::InternalPermissionDenied)
        })
    }

    pub fn service(&self) -> &str {
        self.0.authenticated_service().as_str()
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
            .ok_or(ApiError::Code(ErrorCode::InternalAuthenticationFailed))
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
            // Same operation (signed correlation id), caused by the upstream hop.
            let context = RequestContext {
                request_id: ctx.request_id().clone(),
                correlation_id: ctx.correlation_id().clone(),
                causation_id: Some(CausationId::of_request(ctx.request_id())),
            };
            let span = tracing::info_span!("internal_request", request_id = %context.request_id,
                correlation_id = %context.correlation_id, caller = %ctx.authenticated_service(),
                kid = %ctx.key_id(), method = %request.method(), route = route_pattern(&request));
            request.extensions_mut().insert(InternalCaller(ctx));
            let mut response = scoped(context.clone(), next.run(request).instrument(span)).await;
            if let Ok(value) = HeaderValue::from_str(context.request_id.as_str()) {
                response.headers_mut().insert(headers::REQUEST_ID, value);
            }
            response
        }
        Err(error) => {
            let d = &error.diagnostics;
            crate::metrics::inc(
                "sms_internal_auth_failures_total",
                &[("reason", error.failure.as_str())],
            );
            tracing::warn!(
                reason = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                route = route_pattern(&request),
                "internal service authentication rejected"
            );
            let context = RequestContext::generate();
            let response = scoped(context.clone(), async {
                ApiError::Code(ErrorCode::InternalAuthenticationFailed).into_response()
            })
            .await;
            with_request_id(response, &context)
        }
    }
}
