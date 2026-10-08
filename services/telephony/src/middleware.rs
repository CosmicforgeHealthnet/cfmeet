//! Request context and internal service authentication middleware.
//!
//! Telephony has no customer-facing API: every route except `GET /health` and the provider
//! ingress lives under
//! `/internal/v1/` and passes through [`require_internal_service`] BY CONSTRUCTION —
//! including unknown paths. There is no network-location trust and no customer
//! credential: a request without a valid `X-CF-Service-Authorization` service token is
//! rejected with the normalized `INTERNAL_AUTHENTICATION_FAILED` envelope, whatever else
//! it carries (a customer `Authorization: Bearer cfmk_…` header is refused by the
//! verifier itself).
//!
//! On success the request runs in the signed hop's request / correlation context; the
//! caller is available through [`InternalCaller`]. Diagnostics (reason, kid, issuer) are
//! logged; the token never is.

use std::future::Future;

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{
    headers, CorrelationId, DelegatedPlatformContext, InternalPermission, InternalServiceContext,
    RequestId,
};
use tracing::Instrument;

use crate::{
    error::{ErrorCode, TelephonyError},
    state::AppState,
};

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

/// Runs `fut` with `context` as the current request context.
pub async fn scoped<F: Future>(context: RequestContext, fut: F) -> F::Output {
    CURRENT.scope(context, fut).await
}

/// Public routes (health): a fresh server-generated request id, returned as
/// `x-request-id`. Client-supplied ids are never trusted.
pub async fn request_id_middleware(request: Request, next: Next) -> Response {
    let context = RequestContext::generate();
    let request_id = context.request_id.clone();
    let span = tracing::info_span!(
        "request",
        request_id = %context.request_id,
        method = %request.method(),
        path = %request.uri().path(),
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
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), TelephonyError> {
        self.0.require_permission(permission).map_err(|denied| {
            tracing::warn!(
                caller = %denied.service,
                permission = denied.permission.as_str(),
                "internal permission denied"
            );
            TelephonyError::Refused(ErrorCode::InternalPermissionDenied)
        })
    }

    /// The signed delegated platform context: the ONLY source of tenant / application /
    /// environment scope. Consumer operations without one are refused.
    pub fn delegated(&self) -> Result<&DelegatedPlatformContext, TelephonyError> {
        self.0.delegated_platform_context().ok_or_else(|| {
            tracing::warn!(caller = %self.0.authenticated_service(), "delegated platform context missing");
            TelephonyError::Refused(ErrorCode::InternalPermissionDenied)
        })
    }

    pub fn request_id(&self) -> &RequestId {
        self.0.request_id()
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        self.0.correlation_id()
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for InternalCaller {
    type Rejection = TelephonyError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<InternalCaller>()
            .cloned()
            .ok_or(TelephonyError::Refused(
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
                path = %request.uri().path(),
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
            tracing::warn!(
                reason = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                method = %request.method(),
                path = %request.uri().path(),
                "internal service authentication rejected"
            );
            let context = RequestContext::generate();
            let request_id = context.request_id.clone();
            let mut response = scoped(context, async {
                TelephonyError::Refused(ErrorCode::InternalAuthenticationFailed).into_response()
            })
            .await;
            if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
                response.headers_mut().insert("x-request-id", value);
            }
            response
        }
    }
}
