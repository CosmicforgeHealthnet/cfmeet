//! Internal service authentication middleware and extractor.
//!
//! Every route under `/internal/v1/` — including unknown paths — passes through
//! [`require_internal_service`]. There is no network-location trust: a request without
//! a valid service token is rejected whatever interface it arrived on. Failures return
//! the normalized `INTERNAL_AUTHENTICATION_FAILED` envelope (contract
//! `cfmeet.platform.internal-http`) with a fresh server request id; the specific reason
//! and safe diagnostics (kid, issuer, ids) are logged for operators — never the token.
//!
//! On success the request runs in the signed hop's request / correlation context, and
//! handlers obtain the caller through [`InternalCaller`].

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers, InternalPermission, InternalServiceContext};
use tracing::Instrument;

use crate::{
    domain::RequestContext,
    error::CallingError,
    middleware::request_id::{scoped, REQUEST_ID_HEADER},
    state::AppState,
};

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct InternalCaller(InternalServiceContext);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// Fails with `INTERNAL_PERMISSION_DENIED` unless THIS service's trust policy grants
    /// `permission` to the caller.
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), CallingError> {
        self.0.require_permission(permission).map_err(|denied| {
            tracing::warn!(
                caller = %denied.service,
                permission = denied.permission.as_str(),
                "internal permission denied"
            );
            CallingError::InternalPermissionDenied
        })
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for InternalCaller {
    type Rejection = CallingError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<InternalCaller>()
            .cloned()
            .ok_or(CallingError::InternalAuthenticationFailed)
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
            request.extensions_mut().insert(context.clone());
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
                presented_request_id = d.request_id.as_ref().map(|r| r.as_str()).unwrap_or(""),
                presented_correlation_id = d.correlation_id.as_ref().map(|c| c.as_str()).unwrap_or(""),
                method = %request.method(),
                path = %request.uri().path(),
                "internal service authentication rejected"
            );
            let context = RequestContext::generate();
            let request_id = context.request_id.clone();
            let mut response = scoped(context, async {
                CallingError::InternalAuthenticationFailed.into_response()
            })
            .await;
            if let Ok(value) = HeaderValue::from_str(request_id.as_str()) {
                response.headers_mut().insert(REQUEST_ID_HEADER, value);
            }
            response
        }
    }
}
