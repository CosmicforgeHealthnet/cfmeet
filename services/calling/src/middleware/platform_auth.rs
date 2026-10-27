//! Platform credential authentication middleware.
//!
//! A customer route accepts exactly one of two authentication paths, both answered by
//! Platform Access (the shared identity authority, ADR 0032):
//!
//! 1. **Direct** — `Authorization: Bearer <credential>`, verified by Platform Access
//!    (`/internal/v1/credentials/authenticate`).
//! 2. **Gateway-forwarded** (Part VI §51) — no `Authorization` header and an internal
//!    service token (`X-CF-Service-Authorization`, with the signed request / correlation
//!    id headers) that verifies against Calling's own trust bundle, whose issuer is
//!    granted `calling-request.forward` (grantable only to `cfmeet.api-gateway`), and
//!    whose signed delegated context names the credential the gateway authenticated
//!    (`initiating_credential_id`). Calling then has Platform Access re-verify that
//!    credential live (`/internal/v1/credentials/delegated-authority`); the delegated
//!    identifiers are never trusted as authority on their own. The request continues the
//!    gateway's signed correlation id.
//!
//! The two mechanisms never fall through into each other: a request carrying both
//! headers is refused, a service token from any other issuer (or without the grant, or
//! without the credential id) is refused, and a customer credential is never accepted
//! in the service header. On success either path yields the same [`PlatformAuthContext`]
//! built from the authority chain; it is inserted into the request extensions (handlers
//! extract it), the request-rate limit is enforced, and the platform identifiers are
//! recorded on the request span — identically for both paths. On failure the client
//! receives the normalized `AUTHENTICATION_FAILED` response; the specific bounded reason
//! is logged for operators together with the non-secret public credential id (or, for a
//! rejected service token, the verifier's failure code, kid and issuer). Neither raw
//! header value is ever logged.
//!
//! [`PlatformAuthContext`]: crate::platform::context::PlatformAuthContext
//!
//! Safety-only authority (Part H §72 / §74): a credential whose tenant, application or
//! environment is SUSPENDED authenticates into [`AuthorityMode::SafetyOnly`] and may
//! reach only [`SAFETY_ROUTES`] — operations that stop or remove an effect and create
//! nothing. Every other request receives the same normalized `AUTHENTICATION_FAILED`
//! response as a rejected credential (reason `scope_suspended` in the operator log).

use axum::{
    extract::{MatchedPath, Request, State},
    http::{header, HeaderMap, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};
use cf_service_auth::{DelegatedPlatformContext, InternalPermission, ServiceId};
use zeroize::Zeroizing;

use crate::{
    domain::RequestContext,
    error::CallingError,
    platform::{
        authentication::{AuthFailureReason, AuthenticationError},
        context::AuthorityMode,
        error::summarize_db_error,
        limits::RateDecision,
    },
    state::AppState,
};

/// Header values longer than this are rejected without inspection.
const MAX_AUTHORIZATION_LEN: usize = 256;

/// The safety operations a SUSPENDED scope may still perform (method, axum route
/// template). Each one ends, stops, disables or deletes; none creates a communication,
/// a configuration or a paid effect. Scope checks of the handlers still apply.
pub const SAFETY_ROUTES: &[(&Method, &str)] = &[
    (&Method::POST, "/v1/calls/:call_id/end"),
    (
        &Method::POST,
        "/v1/calls/:call_id/recordings/:recording_id/stop",
    ),
    (
        &Method::DELETE,
        "/v1/enterprise/forwarding-rules/:forwarding_rule_id",
    ),
    (&Method::DELETE, "/v1/enterprise/recordings/:recording_id"),
    (
        &Method::DELETE,
        "/v1/enterprise/voicemail/messages/:message_id",
    ),
    (&Method::PATCH, "/v1/webhooks/:webhook_id"),
    (&Method::DELETE, "/v1/webhooks/:webhook_id"),
];

/// Whether `(method, matched route)` is on the safety allow-list.
pub fn is_safety_route(method: &Method, matched: &str) -> bool {
    SAFETY_ROUTES
        .iter()
        .any(|(m, path)| *m == method && *path == matched)
}

fn bearer_credential(headers: &HeaderMap) -> Option<Zeroizing<String>> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > MAX_AUTHORIZATION_LEN {
        return None;
    }
    let value = value.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let credential = credential.trim();
    (!credential.is_empty()).then(|| Zeroizing::new(credential.to_string()))
}

fn reject(reason: AuthFailureReason, public_id: Option<&str>) -> Response {
    crate::metrics::inc(
        "calling_authentication_failures_total",
        &[("reason", reason.as_str())],
    );
    tracing::info!(
        reason = reason.as_str(),
        public_id = public_id.unwrap_or(""),
        "platform authentication rejected"
    );
    CallingError::AuthenticationFailed.into_response()
}

/// A request forwarded by the API Gateway: its verified delegated context, and the
/// request context continuing the gateway's signed correlation id.
struct GatewayForward {
    delegated: DelegatedPlatformContext,
    request: RequestContext,
}

/// Verifies the service token of a gateway-forwarded request (the caller has checked that
/// no `Authorization` header is present). Every failure is a bounded reason; the token is
/// never logged.
fn gateway_forward(
    state: &AppState,
    headers: &HeaderMap,
    local: &RequestContext,
) -> Result<GatewayForward, AuthFailureReason> {
    let caller = state
        .service_auth
        .verifier()
        .verify_headers(headers)
        .map_err(|error| {
            let d = &error.diagnostics;
            tracing::info!(
                failure = error.failure.as_str(),
                kid = d.kid.as_ref().map(|k| k.as_str()).unwrap_or(""),
                issuer = d.issuer.as_ref().map(|i| i.as_str()).unwrap_or(""),
                "service token on a customer route did not verify"
            );
            AuthFailureReason::ServiceTokenInvalid
        })?;
    // The trust bundle can grant the permission only to the API Gateway; the identity is
    // checked as well so no other issuer can ever reach this path.
    if caller.authenticated_service() != &ServiceId::api_gateway()
        || caller
            .require_permission(InternalPermission::CallingRequestForward)
            .is_err()
    {
        tracing::info!(
            caller = %caller.authenticated_service(),
            kid = %caller.key_id(),
            "service token on a customer route without calling-request.forward"
        );
        return Err(AuthFailureReason::ForwardNotPermitted);
    }
    let delegated = caller
        .delegated_platform_context()
        .filter(|d| d.initiating_credential_id().is_some())
        .cloned()
        .ok_or_else(|| {
            tracing::info!(
                caller = %caller.authenticated_service(),
                "gateway-forwarded request without a delegated credential"
            );
            AuthFailureReason::DelegatedCredentialMissing
        })?;
    Ok(GatewayForward {
        delegated,
        request: RequestContext {
            request_id: local.request_id.clone(),
            correlation_id: caller.correlation_id().clone(),
        },
    })
}

pub async fn require_platform_credential(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_context = request
        .extensions()
        .get::<RequestContext>()
        .cloned()
        .unwrap_or_else(RequestContext::generate);
    let headers = request.headers();
    let authenticated = if headers.contains_key(&cf_service_auth::headers::SERVICE_AUTHORIZATION) {
        // A service token AND a customer credential: never forwarded, never mixed.
        if headers.contains_key(header::AUTHORIZATION) {
            return reject(AuthFailureReason::ServiceCredentialPresented, None);
        }
        let forward = match gateway_forward(&state, headers, &request_context) {
            Ok(forward) => forward,
            Err(reason) => return reject(reason, None),
        };
        state
            .authenticator
            .authenticate_delegated(&forward.delegated, forward.request)
            .await
    } else {
        let Some(credential) = bearer_credential(headers) else {
            return reject(AuthFailureReason::MissingCredential, None);
        };
        state
            .authenticator
            .authenticate(&credential, request_context)
            .await
    };

    let ctx = match authenticated {
        Ok(ctx) => ctx,
        Err(AuthenticationError::Rejected { reason, public_id }) => {
            return reject(reason, public_id.as_deref());
        }
        // §109: a key id above its failed-authentication window is answered 429 (a
        // correct secret is never refused by it; Platform Access keeps the windows).
        Err(AuthenticationError::Throttled {
            reason,
            public_id,
            retry_after_secs,
        }) => {
            crate::metrics::inc(
                "calling_authentication_failures_total",
                &[("reason", "throttled")],
            );
            tracing::warn!(
                reason = reason.as_str(),
                public_id = public_id.as_deref().unwrap_or(""),
                "repeated failed authentication throttled"
            );
            return CallingError::RateLimited { retry_after_secs }.into_response();
        }
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority (Platform Access) unavailable; request refused");
            return CallingError::ServiceUnavailable.into_response();
        }
    };

    if let AuthorityMode::SafetyOnly(link) = ctx.authority_mode() {
        // Only the matched route template decides (never the raw path), and a request
        // that matched no route is not a safety operation.
        let matched = request
            .extensions()
            .get::<MatchedPath>()
            .map(|m| m.as_str().to_string());
        let allowed = matched
            .as_deref()
            .is_some_and(|m| is_safety_route(request.method(), m));
        if !allowed {
            tracing::info!(
                reason = AuthFailureReason::ScopeSuspended.as_str(),
                suspended = link.as_str(),
                credential_id = %ctx.credential_id(),
                "platform authentication rejected: suspended scope, not a safety operation"
            );
            return CallingError::AuthenticationFailed.into_response();
        }
        tracing::info!(
            suspended = link.as_str(),
            credential_id = %ctx.credential_id(),
            "safety operation admitted for a suspended scope"
        );
    }

    let span = tracing::Span::current();
    span.record("tenant_id", tracing::field::display(ctx.tenant_id()));
    span.record(
        "application_id",
        tracing::field::display(ctx.application_id()),
    );
    span.record(
        "environment_id",
        tracing::field::display(ctx.environment_id()),
    );
    span.record(
        "service_account_id",
        tracing::field::display(ctx.service_account_id()),
    );
    span.record(
        "credential_id",
        tracing::field::display(ctx.credential_id()),
    );

    match state.rate_limiter.check(&ctx).await {
        Ok(RateDecision::Allowed) => {}
        Ok(RateDecision::Limited { retry_after_secs }) => {
            return CallingError::RateLimited { retry_after_secs }.into_response();
        }
        Err(err) => {
            tracing::error!(summary = %summarize_db_error(&err), "rate limiter unavailable; request refused");
            return CallingError::ServiceUnavailable.into_response();
        }
    }

    request.extensions_mut().insert(ctx);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(values: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for v in values {
            map.append(header::AUTHORIZATION, HeaderValue::from_str(v).unwrap());
        }
        map
    }

    #[test]
    fn safety_allow_list_is_method_and_template_exact() {
        assert!(is_safety_route(&Method::POST, "/v1/calls/:call_id/end"));
        assert!(!is_safety_route(&Method::GET, "/v1/calls/:call_id/end"));
        assert!(!is_safety_route(&Method::POST, "/v1/calls"));
        assert!(!is_safety_route(
            &Method::POST,
            "/v1/calls/550e8400-e29b-41d4-a716-446655440000/end"
        ));
        assert!(!is_safety_route(
            &Method::POST,
            "/v1/enterprise/forwarding-rules"
        ));
        assert!(is_safety_route(
            &Method::DELETE,
            "/v1/enterprise/forwarding-rules/:forwarding_rule_id"
        ));
    }

    #[test]
    fn extracts_bearer_credentials_only() {
        assert_eq!(
            bearer_credential(&headers(&["Bearer abc"]))
                .as_deref()
                .map(String::as_str),
            Some("abc")
        );
        assert_eq!(
            bearer_credential(&headers(&["bearer   abc  "]))
                .as_deref()
                .map(String::as_str),
            Some("abc")
        );
        assert!(bearer_credential(&headers(&[])).is_none());
        assert!(bearer_credential(&headers(&["Basic abc"])).is_none());
        assert!(bearer_credential(&headers(&["Bearer"])).is_none());
        assert!(bearer_credential(&headers(&["Bearer "])).is_none());
        assert!(bearer_credential(&headers(&["Bearer a", "Bearer b"])).is_none());
        let long = format!("Bearer {}", "a".repeat(MAX_AUTHORIZATION_LEN));
        assert!(bearer_credential(&headers(&[&long])).is_none());
    }
}
