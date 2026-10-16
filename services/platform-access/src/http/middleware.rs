//! Request context and internal service authentication middleware.
//!
//! Platform Access has no customer-facing API: every route except `GET /health` and
//! `GET /ready` lives under `/internal/v1/` and passes through [`require_internal_service`]
//! BY CONSTRUCTION — including unknown paths. A request without a valid
//! `X-CF-Service-Authorization` service token is refused with the normalized
//! `INTERNAL_AUTHENTICATION_FAILED` envelope whatever else it carries (a customer
//! `Authorization: Bearer cfmk_…` header is never an internal credential).
//!
//! On success the request runs in the signed hop's request / correlation context and the
//! caller is available through [`InternalCaller`]. Diagnostics (reason, kid, issuer) are
//! logged; tokens never are. Spans carry the matched ROUTE PATTERN, never the path.

use std::future::Future;

use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{
    headers, CorrelationId, DelegatedPlatformContext, InternalPermission, InternalServiceContext,
    RequestId,
};
use tracing::Instrument;

use super::error::{AccessError, ErrorCode};
use crate::state::AppState;

/// Server-controlled request identity of one request / hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub correlation_id: CorrelationId,
}

impl RequestContext {
    pub fn generate() -> Self {
        Self {
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
        }
    }
}

tokio::task_local! {
    static CURRENT: RequestContext;
}

/// Request id of the request being served, if any.
pub fn current_request_id() -> Option<RequestId> {
    CURRENT.try_with(|c| c.request_id.clone()).ok()
}

/// Correlation id of the request being served, if any.
pub fn current_correlation_id() -> Option<CorrelationId> {
    CURRENT.try_with(|c| c.correlation_id.clone()).ok()
}

/// Runs `fut` with `context` as the current request context.
pub async fn scoped<F: Future>(context: RequestContext, fut: F) -> F::Output {
    CURRENT.scope(context, fut).await
}

fn route_pattern(request: &Request) -> &str {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("<unmatched>")
}

/// Public probe routes: a fresh server-generated request id, returned as `x-request-id`.
pub async fn request_id_middleware(request: Request, next: Next) -> Response {
    let context = RequestContext::generate();
    let request_id = context.request_id.clone();
    let span = tracing::info_span!(
        "request",
        request_id = %context.request_id,
        method = %request.method(),
        route = route_pattern(&request),
    );
    let mut response = scoped(context, next.run(request).instrument(span)).await;
    if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct InternalCaller(InternalServiceContext);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// Fails with `INTERNAL_PERMISSION_DENIED` unless THIS service's trust policy grants
    /// `permission` to the caller (tokens never carry permissions).
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), AccessError> {
        self.0.require_permission(permission).map_err(|denied| {
            crate::metrics::inc(
                "platform_access_permission_denied_total",
                &[("permission", denied.permission.as_str())],
            );
            tracing::warn!(
                caller = %denied.service,
                permission = denied.permission.as_str(),
                "internal permission denied"
            );
            AccessError::Refused(ErrorCode::InternalPermissionDenied)
        })
    }

    /// The signed delegated platform context: the ONLY source of tenant / application /
    /// environment scope. A request body never supplies it.
    pub fn delegated(&self) -> Result<&DelegatedPlatformContext, AccessError> {
        self.0.delegated_platform_context().ok_or_else(|| {
            tracing::warn!(caller = %self.0.authenticated_service(), "delegated platform context missing");
            AccessError::Refused(ErrorCode::DelegatedContextRequired)
        })
    }

    pub fn service(&self) -> &str {
        self.0.authenticated_service().as_str()
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for InternalCaller {
    type Rejection = AccessError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<InternalCaller>()
            .cloned()
            .ok_or(AccessError::Refused(
                ErrorCode::InternalAuthenticationFailed,
            ))
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
            };
            let span = tracing::info_span!(
                "internal_request",
                request_id = %context.request_id,
                correlation_id = %context.correlation_id,
                caller = %ctx.authenticated_service(),
                kid = %ctx.key_id(),
                method = %request.method(),
                route = route_pattern(&request),
            );
            request.extensions_mut().insert(InternalCaller(ctx));
            let request_id = context.request_id.clone();
            let mut response = scoped(context, next.run(request).instrument(span)).await;
            if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
                response.headers_mut().insert(headers::REQUEST_ID, value);
            }
            response
        }
        Err(error) => {
            let d = &error.diagnostics;
            crate::metrics::inc(
                "platform_access_internal_auth_failures_total",
                &[("reason", error.failure.as_str())],
            );
            tracing::warn!(
                reason = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                method = %request.method(),
                route = route_pattern(&request),
                "internal service authentication rejected"
            );
            let context = RequestContext::generate();
            let request_id = context.request_id.clone();
            let mut response = scoped(context, async {
                AccessError::Refused(ErrorCode::InternalAuthenticationFailed).into_response()
            })
            .await;
            if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
                response.headers_mut().insert("x-request-id", value);
            }
            response
        }
    }
}
