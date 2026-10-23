//! Request context and internal service authentication.
//!
//! * Public probes (`/health`, `/ready`, `/openapi.json`) get a fresh server-generated
//!   request context.
//! * `/internal/v1/**`: a valid `X-CF-Service-Authorization` service token whose issuer the
//!   Developer Platform's trust bundle grants the route's permission ([`InternalCaller`]);
//!   the request continues the signed correlation id and records the upstream hop as its
//!   cause. There is no customer (`Authorization`) authentication here: customers reach
//!   the Developer Platform only through the API Gateway.
//!
//! Spans carry the matched ROUTE PATTERN, never the path; tokens are never logged.

use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{headers, InternalPermission, InternalServiceContext};
use tracing::Instrument;

use super::error::{ApiError, ErrorCode};
use crate::{
    domain::{scoped, CausationId, RequestContext},
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
    let span = tracing::info_span!("request", request_id = %context.request_id,
        method = %request.method(), route = route_pattern(&request));
    let response = scoped(context.clone(), next.run(request).instrument(span)).await;
    with_request_id(response, &context)
}

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct InternalCaller(InternalServiceContext);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// Fails with INTERNAL_PERMISSION_DENIED unless the trust bundle grants `permission`.
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.0.require_permission(permission).map_err(|denied| {
            tracing::warn!(caller = %denied.service, permission = denied.permission.as_str(),
                "internal permission denied");
            ApiError(ErrorCode::InternalPermissionDenied)
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
            .ok_or(ApiError(ErrorCode::InternalAuthenticationFailed))
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
            tracing::warn!(
                reason = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                route = route_pattern(&request),
                "internal service authentication rejected"
            );
            let context = RequestContext::generate();
            let response = scoped(context.clone(), async {
                ApiError(ErrorCode::InternalAuthenticationFailed).into_response()
            })
            .await;
            with_request_id(response, &context)
        }
    }
}
