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
//! Operator attribution (ADR 0068): `X-CF-Operator` / `X-CF-Approver` /
//! `X-CF-Approval-Ref` / `X-CF-Intent-Digest` are parsed once here.
//! * from `cfmeet.admin-bridge`: strict `hq:<staff>` attribution
//!   (`cf_service_auth::OperatorAttribution`), required on every bridge request;
//! * from the operator console: the legacy named operator (`X-CF-Operator` only, checked
//!   by the operator routes); the approval headers are refused;
//! * from any other caller: any of the four headers is refused.
//!
//! A refusal is `403 INTERNAL_PERMISSION_DENIED`, so no caller can claim an operator.
//!
//! Spans carry the matched ROUTE PATTERN, never the path; tokens are never logged.

use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{
    headers, operator as attribution, services, InternalPermission, InternalServiceContext,
    OperatorAttribution,
};
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
pub struct InternalCaller(InternalServiceContext, Option<OperatorAttribution>);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// The admin bridge's verified HQ operator attribution (`None` for every other caller).
    pub fn bridge_operator(&self) -> Option<&OperatorAttribution> {
        self.1.as_ref()
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

/// Applies the attribution rule of this module to a verified caller.
fn attribution_of(
    ctx: &InternalServiceContext,
    headers: &axum::http::HeaderMap,
) -> Result<Option<OperatorAttribution>, &'static str> {
    let caller = ctx.authenticated_service();
    if caller.as_str() == services::ADMIN_BRIDGE {
        return OperatorAttribution::from_headers(caller, headers)
            .map_err(|_| "bridge attribution malformed or incomplete");
    }
    let present = |h: &axum::http::HeaderName| headers.contains_key(h);
    if caller.as_str() == services::OPERATOR_CONSOLE {
        if [
            &attribution::APPROVER,
            &attribution::APPROVAL_REF,
            &attribution::INTENT_DIGEST,
        ]
        .into_iter()
        .any(present)
        {
            return Err("approval attribution is accepted only from the admin bridge");
        }
        return Ok(None);
    }
    if attribution::ALL_HEADERS.iter().any(present) {
        return Err("operator attribution from a caller that may not claim an operator");
    }
    Ok(None)
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
                kid = %ctx.key_id(), method = %request.method(), route = route_pattern(&request));
            let operator = match attribution_of(&ctx, request.headers()) {
                Ok(o) => o,
                Err(reason) => {
                    tracing::warn!(caller = %ctx.authenticated_service(), reason,
                        route = route_pattern(&request), "operator attribution refused");
                    let response = scoped(context.clone(), async {
                        ApiError(ErrorCode::InternalPermissionDenied).into_response()
                    })
                    .await;
                    return with_request_id(response, &context);
                }
            };
            request
                .extensions_mut()
                .insert(InternalCaller(ctx, operator));
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
