//! Request context and authentication.
//!
//! * Public probes (`/health`, `/ready`) get a fresh request context.
//! * `/v1/**` (customer): `Authorization: Bearer <credential>` verified by Platform
//!   Access on every request ([`CustomerCaller`]); the tenant / application / environment
//!   come ONLY from the authority chain. Every failure is the same 401; throttling is a
//!   429; the authority unreachable is a 503 (fail closed). A service token is never a
//!   customer credential.
//! * Gateway-forwarded customer requests (Part VI §51), ONLY on the customer numbers
//!   family (`/v1/numbers/**` route patterns — never `/v1/admin/**`, unknown paths or any
//!   other surface): the API Gateway authenticated the customer's credential and forwards
//!   the request WITHOUT `Authorization`, carrying its own signed service token instead.
//!   Numbering verifies that token with its trust bundle, requires
//!   `numbering-request.forward` and a delegated context naming the credential
//!   (`initiating_credential_id`), and asks Platform Access to re-verify that credential
//!   live (`delegated-authority`). The returned chain must be exactly the delegated scope;
//!   from there the request is admitted EXACTLY like a bearer request (the same SAFETY_ONLY
//!   refusal, throttling, scopes and [`CustomerCaller`], see [`admit`]) and continues the
//!   gateway's signed correlation id. A token AND an `Authorization` header, a token on any
//!   other route, an untrusted / unpermitted issuer or a missing credential id are the
//!   same normalized 401; the authority unreachable is the same 503. Administration
//!   additionally refuses a gateway-forwarded caller ([`AdminCaller`]).
//! * `/internal/v1/**` (services): a valid `X-CF-Service-Authorization` service token
//!   whose issuer Numbering's trust bundle grants the route's permission
//!   ([`InternalCaller`]); the request continues the signed correlation id and records the
//!   upstream request as its cause.
//!
//! Spans carry the matched ROUTE PATTERN, never the path; credentials and tokens are never
//! logged.

use axum::{
    extract::{FromRequestParts, MatchedPath, Request, State},
    http::{header, request::Parts, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{
    headers, DelegatedPlatformContext, InternalPermission, InternalServiceContext,
};
use tracing::Instrument;

use super::error::{ApiError, ErrorCode};
use crate::{
    domain::{scoped, CausationId, RequestContext},
    identity::{Authentication, AuthorityUnavailable, TrustedContext},
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
            crate::metrics::inc("numbering_scope_denied_total", &[]);
            tracing::info!(scope, "scope check denied");
            Err(ApiError::Code(ErrorCode::InsufficientScope))
        }
    }
}

/// The privileged scope of Numbering administration (catalog, manual KYC review,
/// revocations). Platform Access grants it only to FIRST_PARTY applications by an explicit
/// operator grant.
pub const NUMBERS_ADMIN: &str = "numbers.admin";

/// The audited human operator acting through a `numbers.admin` credential.
pub const OPERATOR_HEADER: &str = "x-cf-operator";

/// An authenticated `numbers.admin` caller and the human operator it acts for.
#[derive(Debug, Clone)]
pub struct AdminCaller {
    pub context: TrustedContext,
    /// `Actor::operator(<X-CF-Operator>)`.
    pub actor: crate::numbers::Actor,
}

fn valid_operator(value: &str) -> bool {
    (1..=100).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'@' | b'-'))
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for AdminCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let caller = parts
            .extensions
            .get::<CustomerCaller>()
            .cloned()
            .ok_or(ApiError::Code(ErrorCode::AuthenticationFailed))?;
        // Administration is never reachable through the API Gateway (defence in depth:
        // the gateway branch is admitted only on `/v1/numbers/**` routes).
        if parts.extensions.get::<GatewayForwarded>().is_some() {
            tracing::warn!("administration refused (gateway-forwarded caller)");
            return Err(ApiError::Code(ErrorCode::AuthenticationFailed));
        }
        if !caller.0.first_party || !caller.0.has_scope(NUMBERS_ADMIN) {
            crate::metrics::inc("numbering_scope_denied_total", &[]);
            tracing::warn!(
                "administration denied (numbers.admin on a first-party application required)"
            );
            return Err(ApiError::Code(ErrorCode::InsufficientScope));
        }
        let mut values = parts.headers.get_all(OPERATOR_HEADER).iter();
        let operator = match (values.next(), values.next()) {
            (Some(v), None) => v
                .to_str()
                .ok()
                .filter(|v| valid_operator(v))
                .map(str::to_string),
            _ => None,
        }
        .ok_or(ApiError::Code(ErrorCode::OperatorRequired))?;
        Ok(AdminCaller {
            actor: crate::numbers::Actor::operator(&operator),
            context: caller.0,
        })
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

/// Marks a customer request the API Gateway forwarded (Part VI §51): set next to the
/// [`CustomerCaller`] by [`require_customer`]; [`AdminCaller`] refuses such a caller.
#[derive(Debug, Clone, Copy)]
pub struct GatewayForwarded;

/// The only route patterns a gateway-forwarded request may reach: the customer numbers
/// family. Matched against the ROUTE PATTERN (an unmatched path has none), never the raw
/// path.
const GATEWAY_ROUTE_PREFIX: &str = "/v1/numbers/";

struct Forwarded {
    token: InternalServiceContext,
    delegated: DelegatedPlatformContext,
}

/// How a `/v1` request presents its customer.
enum Presented {
    /// `Authorization: Bearer <credential>`.
    Bearer(String),
    /// A verified API Gateway token (with `numbering-request.forward`) and its delegated
    /// context naming the credential.
    Gateway(Box<Forwarded>),
    /// Not authenticatable; the reason is for logs and metrics only.
    Refused(&'static str),
}

fn presented(state: &AppState, request: &Request) -> Presented {
    if !request
        .headers()
        .contains_key(headers::SERVICE_AUTHORIZATION)
    {
        return match bearer(request) {
            Some(token) => Presented::Bearer(token),
            None => Presented::Refused("missing_credential"),
        };
    }
    if request.headers().contains_key(header::AUTHORIZATION) {
        return Presented::Refused("service_token_with_authorization");
    }
    if !route_pattern(request).starts_with(GATEWAY_ROUTE_PREFIX) {
        return Presented::Refused("service_token_outside_gateway_routes");
    }
    let token = match state
        .service_auth
        .verifier()
        .verify_headers(request.headers())
    {
        Ok(token) => token,
        Err(error) => {
            let d = &error.diagnostics;
            tracing::warn!(
                reason = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                "gateway service token rejected"
            );
            return Presented::Refused(error.failure.as_str());
        }
    };
    if let Err(denied) = token.require_permission(InternalPermission::NumberingRequestForward) {
        tracing::warn!(
            caller = %denied.service,
            permission = denied.permission.as_str(),
            "gateway forwarding permission denied"
        );
        return Presented::Refused("forward_permission_denied");
    }
    match token.delegated_platform_context().cloned() {
        Some(delegated) if delegated.initiating_credential_id().is_some() => {
            Presented::Gateway(Box::new(Forwarded { token, delegated }))
        }
        _ => Presented::Refused("delegated_credential_missing"),
    }
}

pub async fn require_customer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let presented = presented(&state, &request);
    let context = match &presented {
        // The gateway's operation (signed correlation id), caused by its forwarding hop.
        Presented::Gateway(forwarded) => RequestContext {
            request_id: forwarded.token.request_id().clone(),
            correlation_id: forwarded.token.correlation_id().clone(),
            causation_id: Some(CausationId::of_request(forwarded.token.request_id())),
        },
        _ => RequestContext::generate(),
    };
    let via_gateway = matches!(presented, Presented::Gateway(_));
    let span = tracing::info_span!("customer_request", request_id = %context.request_id,
        correlation_id = %context.correlation_id, via_gateway, method = %request.method(),
        route = route_pattern(&request));
    let response = scoped(
        context.clone(),
        async move {
            match presented {
                Presented::Refused(reason) => {
                    crate::metrics::inc(
                        "numbering_customer_auth_refused_total",
                        &[("reason", reason)],
                    );
                    tracing::info!(reason, "customer authentication refused");
                    ApiError::Code(ErrorCode::AuthenticationFailed).into_response()
                }
                Presented::Bearer(token) => {
                    let outcome = state.authority.authenticate(&token).await;
                    admit(outcome, request, next).await
                }
                Presented::Gateway(forwarded) => {
                    let Forwarded { token, delegated } = *forwarded;
                    let outcome = state
                        .authority
                        .authenticate_delegated(&delegated, token.correlation_id())
                        .await
                        .map(|authentication| match authentication {
                            Authentication::Authenticated(ctx)
                                if !ctx.is_delegated_by(&delegated) =>
                            {
                                tracing::warn!(
                                    caller = %token.authenticated_service(),
                                    "delegated authority chain does not match the delegated context"
                                );
                                Authentication::Rejected {
                                    reason: "delegated_scope_mismatch".into(),
                                    throttle: None,
                                }
                            }
                            other => other,
                        });
                    let mut request = request;
                    request.extensions_mut().insert(GatewayForwarded);
                    admit(outcome, request, next).await
                }
            }
        }
        .instrument(span),
    )
    .await;
    with_request_id(response, &context)
}

/// Everything after the identity authority answered — identical for a presented and a
/// gateway-forwarded credential: only a FULL chain becomes the [`CustomerCaller`]; a
/// suspended (SAFETY_ONLY) chain and every rejection are the same 401, throttling a 429,
/// the authority unreachable a 503.
async fn admit(
    outcome: Result<Authentication, AuthorityUnavailable>,
    mut request: Request,
    next: Next,
) -> Response {
    match outcome {
        Ok(Authentication::Authenticated(ctx)) if ctx.full_authority => {
            request.extensions_mut().insert(CustomerCaller(*ctx));
            next.run(request).await
        }
        Ok(Authentication::Authenticated(_)) => {
            tracing::info!("customer authentication refused (suspended scope)");
            ApiError::Code(ErrorCode::AuthenticationFailed).into_response()
        }
        Ok(Authentication::Rejected { reason, throttle }) => {
            crate::metrics::inc("numbering_customer_auth_rejected_total", &[]);
            tracing::info!(%reason, "customer authentication rejected");
            match throttle {
                Some(t) => ApiError::RateLimited {
                    retry_after_seconds: t.retry_after_seconds,
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

/// The authenticated internal caller of the current request.
#[derive(Debug, Clone)]
pub struct InternalCaller(InternalServiceContext);

impl InternalCaller {
    pub fn context(&self) -> &InternalServiceContext {
        &self.0
    }

    /// Fails with INTERNAL_PERMISSION_DENIED unless Numbering's trust bundle grants
    /// `permission` to the caller.
    pub fn require_permission(&self, permission: InternalPermission) -> Result<(), ApiError> {
        self.0.require_permission(permission).map_err(|denied| {
            crate::metrics::inc("numbering_permission_denied_total", &[("permission", denied.permission.as_str())]);
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
                "numbering_internal_auth_failures_total",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn admin_parts(forwarded: bool) -> Parts {
        let id = || uuid::Uuid::new_v4().to_string();
        let context = TrustedContext::from_authority(&serde_json::json!({
            "authorityMode": "FULL",
            "tenant": { "id": id(), "classification": "FIRST_PARTY" },
            "application": { "id": id() },
            "environment": { "id": id(), "kind": "PRODUCTION" },
            "serviceAccount": { "id": id() },
            "credential": { "id": id() },
            "scopes": [NUMBERS_ADMIN],
        }))
        .unwrap();
        let (mut parts, ()) = axum::http::Request::get("/v1/admin/controls")
            .header(OPERATOR_HEADER, "ops.alice")
            .body(())
            .unwrap()
            .into_parts();
        parts.extensions.insert(CustomerCaller(context));
        if forwarded {
            parts.extensions.insert(GatewayForwarded);
        }
        parts
    }

    #[tokio::test]
    async fn administration_refuses_a_gateway_forwarded_caller() {
        assert!(
            AdminCaller::from_request_parts(&mut admin_parts(false), &())
                .await
                .is_ok()
        );
        let refused = AdminCaller::from_request_parts(&mut admin_parts(true), &()).await;
        assert!(matches!(
            refused,
            Err(ApiError::Code(ErrorCode::AuthenticationFailed))
        ));
    }
}
