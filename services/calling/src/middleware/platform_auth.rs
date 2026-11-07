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
//! **Organization-forwarded** (Blueprint Section 12 follow-up, CAL-007; OFF unless
//! `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`) — the same service-token path, but the
//! issuer is `cfmeet.meeting-service` holding `calling-request.act-for-organization`
//! (grantable only to the Meeting Service, which authenticated the human and checked the
//! organization's `calling.enterprise.manage` capability with the Organizations Service).
//! The delegated context must name the organization's Calling credential (id only) AND a
//! NATIVE_CF_USER acting principal. The credential is re-verified live with Platform
//! Access exactly like a gateway-forwarded one; then the tenant must be FIRST_PARTY with
//! the reserved organization slug (`org-<32 hex>`) and the acting principal must resolve,
//! ACTIVE and NATIVE_CF_USER, inside the credential's tenant / application / environment.
//! The principal is recorded on the context (`acting_principal_id`) so enterprise
//! administrative audit names the human. With the flag off, a Meeting Service token is
//! refused like any other non-gateway issuer (`forward_not_permitted`).
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
use cf_service_auth::{
    DelegatedPlatformContext, DelegatedPrincipalKind, InternalPermission, ServiceId,
};
use zeroize::Zeroizing;

use crate::{
    domain::RequestContext,
    domain::{PrincipalId, PrincipalKind, TenantClassification},
    error::CallingError,
    platform::{
        authentication::{AuthFailureReason, AuthenticationError},
        context::{AuthorityMode, PlatformAuthContext},
        error::summarize_db_error,
        limits::RateDecision,
        principals::{PrincipalReference, PrincipalResolutionError},
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

/// Which service forwarded a request on the service-token path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForwardKind {
    /// The API Gateway (Part VI §51, `calling-request.forward`).
    Gateway,
    /// The Meeting Service for a first-party organization member
    /// (`calling-request.act-for-organization`, flag-gated).
    Organization,
}

/// Whether `issuer` is a service the shared permission registry allows to hold
/// `calling-request.act-for-organization` (exactly one: the account authority that
/// authenticated the human member). Derived from the registry so Calling never names —
/// or couples to — that service's code.
fn may_act_for_organization(issuer: &ServiceId) -> bool {
    InternalPermission::CallingRequestActForOrganization
        .definition()
        .grantable_to
        .contains(&issuer.as_str())
}

/// Which forwarding path a verified service token may use: the identity AND the grant are
/// both required (the trust bundle grants each permission to one service only; the
/// identity check keeps any other issuer out even if a bundle were misconfigured). The
/// organization path exists only while `organization_forwarding_enabled` is set; without
/// it the organization forwarder's token is refused exactly like any non-gateway issuer.
pub(crate) fn forward_kind(
    issuer: &ServiceId,
    has_forward_grant: bool,
    has_organization_grant: bool,
    organization_forwarding_enabled: bool,
) -> Result<ForwardKind, AuthFailureReason> {
    if issuer == &ServiceId::api_gateway() && has_forward_grant {
        return Ok(ForwardKind::Gateway);
    }
    if organization_forwarding_enabled && may_act_for_organization(issuer) && has_organization_grant
    {
        return Ok(ForwardKind::Organization);
    }
    Err(AuthFailureReason::ForwardNotPermitted)
}

/// The delegated context a forwarding path requires: always the credential the request
/// acts with; for an organization request also a NATIVE_CF_USER acting principal. Returns
/// that principal (organization path only).
pub(crate) fn required_delegation(
    kind: ForwardKind,
    delegated: &DelegatedPlatformContext,
) -> Result<Option<uuid::Uuid>, AuthFailureReason> {
    if delegated.initiating_credential_id().is_none() {
        return Err(AuthFailureReason::DelegatedCredentialMissing);
    }
    match kind {
        ForwardKind::Gateway => Ok(None),
        ForwardKind::Organization => match delegated.initiating_principal() {
            Some(p) if p.principal_kind == DelegatedPrincipalKind::NativeCfUser => {
                Ok(Some(p.principal_id))
            }
            _ => Err(AuthFailureReason::ActingPrincipalMissing),
        },
    }
}

/// A request forwarded on the service-token path: its verified delegated context, the
/// forwarding kind, the acting principal (organization path) and the request context
/// continuing the forwarder's signed correlation id.
struct GatewayForward {
    delegated: DelegatedPlatformContext,
    kind: ForwardKind,
    acting_principal: Option<uuid::Uuid>,
    request: RequestContext,
}

/// Verifies the service token of a forwarded request (the caller has checked that no
/// `Authorization` header is present). Every failure is a bounded reason; the token is
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
    let kind = forward_kind(
        caller.authenticated_service(),
        caller
            .require_permission(InternalPermission::CallingRequestForward)
            .is_ok(),
        caller
            .require_permission(InternalPermission::CallingRequestActForOrganization)
            .is_ok(),
        state.organization_forwarding_enabled,
    )
    .inspect_err(|_| {
        tracing::info!(
            caller = %caller.authenticated_service(),
            kid = %caller.key_id(),
            "service token on a customer route without a forwarding grant"
        );
    })?;
    let delegated = caller
        .delegated_platform_context()
        .cloned()
        .ok_or_else(|| {
            tracing::info!(
                caller = %caller.authenticated_service(),
                "forwarded request without a delegated credential"
            );
            AuthFailureReason::DelegatedCredentialMissing
        })?;
    let acting_principal = required_delegation(kind, &delegated).inspect_err(|reason| {
        tracing::info!(
            caller = %caller.authenticated_service(),
            reason = reason.as_str(),
            "forwarded request without the required delegated context"
        );
    })?;
    Ok(GatewayForward {
        delegated,
        kind,
        acting_principal,
        request: RequestContext {
            request_id: local.request_id.clone(),
            correlation_id: caller.correlation_id().clone(),
        },
    })
}

/// Organization-forwarded requests, after Platform Access re-verified the credential: the
/// tenant must be a FIRST_PARTY organization tenant and the acting principal an ACTIVE
/// NATIVE_CF_USER of the very same tenant / application / environment. An unreachable
/// principal directory refuses with 503 (fail closed), anything else with the normalized
/// 401.
async fn organization_context(
    state: &AppState,
    ctx: PlatformAuthContext,
    acting_principal: uuid::Uuid,
) -> Result<PlatformAuthContext, Box<Response>> {
    if ctx.tenant_classification() != TenantClassification::FirstParty
        || !ctx.is_organization_tenant()
    {
        return Err(Box::new(reject(
            AuthFailureReason::NotOrganizationScope,
            None,
        )));
    }
    match state
        .principals
        .resolve(
            &ctx,
            &PrincipalReference::Id(PrincipalId::from_uuid(acting_principal)),
        )
        .await
    {
        Ok(p) if p.kind() == PrincipalKind::NativeCfUser => {
            Ok(ctx.with_acting_principal(p.id().as_uuid()))
        }
        Ok(_)
        | Err(PrincipalResolutionError::NotFound)
        | Err(PrincipalResolutionError::Inactive)
        | Err(PrincipalResolutionError::InvalidReference) => Err(Box::new(reject(
            AuthFailureReason::ActingPrincipalOutOfScope,
            None,
        ))),
        Err(PrincipalResolutionError::Unavailable { summary }) => {
            tracing::error!(%summary, "principal directory unavailable; organization request refused");
            Err(Box::new(CallingError::ServiceUnavailable.into_response()))
        }
    }
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
        let authenticated = state
            .authenticator
            .authenticate_delegated(&forward.delegated, forward.request)
            .await;
        match (forward.kind, authenticated) {
            (ForwardKind::Organization, Ok(ctx)) => {
                // `required_delegation` already guaranteed the principal; never accept an
                // organization request without the organization checks.
                let Some(principal) = forward.acting_principal else {
                    return reject(AuthFailureReason::ActingPrincipalMissing, None);
                };
                match organization_context(&state, ctx, principal).await {
                    Ok(ctx) => Ok(ctx),
                    Err(response) => return *response,
                }
            }
            (_, other) => other,
        }
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

    fn delegated() -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            cf_service_auth::ApplicationEnvironmentKind::Production,
            uuid::Uuid::new_v4(),
        )
    }

    #[test]
    fn only_the_gateway_and_the_flag_gated_organization_forwarder_may_forward() {
        let gateway = ServiceId::api_gateway();
        let forwarders = InternalPermission::CallingRequestActForOrganization
            .definition()
            .grantable_to;
        assert_eq!(forwarders.len(), 1);
        let meeting = ServiceId::parse(forwarders[0]).unwrap();
        let other = ServiceId::parse("cfmeet.messaging").unwrap();
        // The gateway path is unchanged and independent of the organization flag.
        for enabled in [false, true] {
            assert_eq!(
                forward_kind(&gateway, true, false, enabled),
                Ok(ForwardKind::Gateway)
            );
            assert_eq!(
                forward_kind(&gateway, false, true, enabled),
                Err(AuthFailureReason::ForwardNotPermitted)
            );
        }
        // Flag off: a Meeting Service token is refused like any other issuer.
        assert_eq!(
            forward_kind(&meeting, false, true, false),
            Err(AuthFailureReason::ForwardNotPermitted)
        );
        assert_eq!(
            forward_kind(&meeting, false, true, true),
            Ok(ForwardKind::Organization)
        );
        // Missing grant, or only the gateway grant on the Meeting Service.
        assert_eq!(
            forward_kind(&meeting, false, false, true),
            Err(AuthFailureReason::ForwardNotPermitted)
        );
        assert_eq!(
            forward_kind(&meeting, true, false, true),
            Err(AuthFailureReason::ForwardNotPermitted)
        );
        // Any other issuer, whatever it claims to hold.
        assert_eq!(
            forward_kind(&other, true, true, true),
            Err(AuthFailureReason::ForwardNotPermitted)
        );
    }

    #[test]
    fn organization_forwarding_requires_credential_and_native_acting_principal() {
        let principal = uuid::Uuid::new_v4();
        let credential = uuid::Uuid::new_v4();
        // No credential id: refused on both paths.
        for kind in [ForwardKind::Gateway, ForwardKind::Organization] {
            assert_eq!(
                required_delegation(kind, &delegated()),
                Err(AuthFailureReason::DelegatedCredentialMissing)
            );
        }
        let with_credential = delegated().with_initiating_credential(credential);
        assert_eq!(
            required_delegation(ForwardKind::Gateway, &with_credential),
            Ok(None)
        );
        assert_eq!(
            required_delegation(ForwardKind::Organization, &with_credential),
            Err(AuthFailureReason::ActingPrincipalMissing)
        );
        let service_principal = with_credential
            .clone()
            .with_initiating_principal(principal, DelegatedPrincipalKind::ServiceAccount);
        assert_eq!(
            required_delegation(ForwardKind::Organization, &service_principal),
            Err(AuthFailureReason::ActingPrincipalMissing)
        );
        let member = with_credential
            .with_initiating_principal(principal, DelegatedPrincipalKind::NativeCfUser);
        assert_eq!(
            required_delegation(ForwardKind::Organization, &member),
            Ok(Some(principal))
        );
        // The gateway path ignores a principal (unchanged behaviour).
        assert_eq!(required_delegation(ForwardKind::Gateway, &member), Ok(None));
    }

    #[test]
    fn organization_tenant_slug_is_the_reserved_form_only() {
        use crate::platform::authentication::is_organization_tenant_slug as org;
        assert!(org("org-0123456789abcdef0123456789abcdef"));
        for bad in [
            "org-0123456789ABCDEF0123456789abcdef",
            "org-0123456789abcdef0123456789abcde",
            "org-0123456789abcdef0123456789abcdef0",
            "org-0123456789abcdef0123456789abcdeg",
            "cfmeet",
            "org-",
            "xorg-0123456789abcdef0123456789abcdef",
        ] {
            assert!(!org(bad), "{bad}");
        }
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
