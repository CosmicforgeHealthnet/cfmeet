//! Device exchange of a CF Meet user token for a calling endpoint and a signaling
//! credential (ADR 0062 decision 3; `docs/platform/build-internal-contracts.md` §4, §7).
//!
//! `POST /v1/calling/user-sessions` with `Authorization: Bearer cfut_…` and
//! `{installationId, kind: "WEB" | "MOBILE"}` answers
//! `{principalId, endpointId, signalingCredential, signalingCredentialExpiresAt}`.
//! Mounted only while `CALLING_USER_TOKEN_EXCHANGE_ENABLED=true` (default off: the path
//! does not exist).
//!
//! 1. The token is verified live by Platform Access ([`UserTokenVerifier`], ≤ 30 s reuse).
//!    Every refusal is the uniform 401; an unreachable authority is 503.
//! 2. The token must hold the effective grant `call:receive` (403 otherwise).
//! 3. The token's minting credential (`crd`, with `sva` and the scope triple) is
//!    re-verified LIVE exactly like a gateway-forwarded request
//!    ([`CredentialAuthenticator::authenticate_delegated`]): its chain must be fully ACTIVE,
//!    its tenant EXTERNAL and it must hold `calls.write`; the request counts toward that
//!    credential's rate limit.
//! 4. The token's user must be an ACTIVE EXTERNAL_APPLICATION_PRINCIPAL of that
//!    environment.
//! 5. The endpoint keyed `inst:<installationId>` is created or reused for the principal
//!    ([`EndpointService::register`]). An installation is one app install: when the
//!    ACTIVE endpoint of that key belongs to another principal of the same environment (a
//!    different user signed in on the same install) or has another kind, it is revoked
//!    (its live offers end) and the installation's endpoint is registered anew.
//! 6. A `cfsig_` signaling credential is minted for that endpoint with the token's `crd`
//!    and `sva`, so the existing per-use check (endpoint, principal and minting credential
//!    chain still valid, credential holding `calls.write`) keeps working, and it never
//!    outlives the user token.
//!
//! [`CredentialAuthenticator::authenticate_delegated`]:
//!     crate::platform::authentication::CredentialAuthenticator::authenticate_delegated
//! [`EndpointService::register`]: super::EndpointService::register

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::post,
    Router,
};
use cf_service_auth::{ApplicationEnvironmentKind, DelegatedPlatformContext};
use chrono::SecondsFormat;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{EndpointCapabilities, EndpointKey, EndpointKind, NewEndpoint};
use crate::{
    calls::{app::http::respond, command::CallScope, errors::CallError},
    domain::{
        EnvironmentKind, PrincipalId, PrincipalKind, PrincipalStatus, RequestContext,
        TenantClassification,
    },
    error::CallingError,
    platform::{
        authentication::AuthenticationError,
        limits::RateDecision,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::names,
        user_tokens::{UserTokenError, TOKEN_PREFIX},
    },
    signaling::SignalingCredentialError,
    state::AppState,
};

/// The route (registered by `routes::app` only while the exchange is enabled).
pub const PATH: &str = "/v1/calling/user-sessions";
/// The effective grant the exchange requires (reported in `WWW-Authenticate`).
pub const REQUIRED_GRANT: &str = "call:receive";
const BODY_LIMIT: usize = 4 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calling/user-sessions", post(exchange))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExchangeBody {
    installation_id: String,
    kind: DeviceKind,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum DeviceKind {
    Web,
    Mobile,
}

impl DeviceKind {
    fn endpoint_kind(self) -> EndpointKind {
        match self {
            DeviceKind::Web => EndpointKind::Web,
            DeviceKind::Mobile => EndpointKind::Mobile,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExchangeView {
    principal_id: Uuid,
    endpoint_id: Uuid,
    signaling_credential: String,
    signaling_credential_expires_at: String,
}

/// `installationId`: 8–128 characters of `[A-Za-z0-9._-]` (contract §4).
pub fn valid_installation_id(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The endpoint key of an installation: `inst:<installationId>`; an id too long for the
/// 128-character endpoint key becomes `inst:h:<sha256 hex of the id>` (an installation id
/// never contains `:`, so the two forms never collide).
pub fn installation_key(installation_id: &str) -> Option<EndpointKey> {
    if !valid_installation_id(installation_id) {
        return None;
    }
    let direct = format!("inst:{installation_id}");
    if direct.len() <= 128 {
        return EndpointKey::parse(&direct);
    }
    EndpointKey::parse(&format!(
        "inst:h:{}",
        hex::encode(Sha256::digest(installation_id.as_bytes()))
    ))
}

/// `Authorization: Bearer cfut_…` (exactly one header; anything else is refused).
fn user_token(headers: &HeaderMap) -> Option<String> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() || value.len() > 2_100 {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && token.starts_with(TOKEN_PREFIX))
        .then(|| token.to_string())
}

fn refused(reason: &'static str) -> CallingError {
    crate::metrics::inc(
        "calling_user_session_exchanges_total",
        &[("outcome", reason)],
    );
    tracing::info!(reason, "user-session exchange refused");
    match reason {
        "unavailable" => CallingError::ServiceUnavailable,
        "grant" => CallingError::InsufficientScope {
            required: REQUIRED_GRANT.to_string(),
        },
        "scope" => CallingError::InsufficientScope {
            required: names::CALLS_WRITE.to_string(),
        },
        _ => CallingError::AuthenticationFailed,
    }
}

fn application_environment_kind(kind: EnvironmentKind) -> ApplicationEnvironmentKind {
    match kind {
        EnvironmentKind::Sandbox => ApplicationEnvironmentKind::Sandbox,
        EnvironmentKind::Test => ApplicationEnvironmentKind::Test,
        EnvironmentKind::Production => ApplicationEnvironmentKind::Production,
    }
}

async fn exchange(State(state): State<AppState>, request: Request) -> Result<Response, CallingError> {
    let Some(verifier) = state.user_sessions.clone() else {
        return Err(CallingError::NotFound);
    };
    let (parts, body) = request.into_parts();
    // A service token never reaches a device route (the gateway passes USER_TOKEN
    // requests through unchanged, without one).
    if parts
        .headers
        .contains_key(&cf_service_auth::headers::SERVICE_AUTHORIZATION)
    {
        return Err(refused("service_token"));
    }
    let Some(token) = user_token(&parts.headers) else {
        return Err(refused("missing_token"));
    };
    let request_context = parts
        .extensions
        .get::<RequestContext>()
        .cloned()
        .unwrap_or_else(RequestContext::generate);

    // 1. The token, verified live by Platform Access.
    let verified = match verifier
        .verify(&token, &request_context.correlation_id)
        .await
    {
        Ok(v) => v,
        Err(UserTokenError::Rejected(_)) => return Err(refused("token")),
        Err(UserTokenError::Unavailable(summary)) => {
            tracing::error!(%summary, "user token verification unavailable");
            return Err(refused("unavailable"));
        }
    };
    drop(token);
    if verified.user.status != PrincipalStatus::Active {
        return Err(refused("user_inactive"));
    }
    // 2. The call grant.
    if !verified.grants.may_receive_calls() {
        return Err(refused("grant"));
    }
    // 3. The minting credential's LIVE authority chain.
    let delegated = DelegatedPlatformContext::new(
        verified.tenant_id,
        verified.application_id,
        verified.environment_id,
        application_environment_kind(verified.environment_kind),
        verified.service_account_id,
    )
    .with_initiating_credential(verified.credential_id);
    let ctx = match state
        .authenticator
        .authenticate_delegated(&delegated, request_context)
        .await
    {
        Ok(ctx) => ctx,
        Err(AuthenticationError::Unavailable { summary }) => {
            tracing::error!(%summary, "credential authority unavailable");
            return Err(refused("unavailable"));
        }
        Err(_) => return Err(refused("minting_credential")),
    };
    if ctx.is_safety_only() {
        return Err(refused("scope_suspended"));
    }
    // User tokens exist for EXTERNAL tenants only (contract §1): a FIRST_PARTY chain is
    // never a valid user-token authority.
    if ctx.tenant_classification() != TenantClassification::External {
        return Err(refused("not_external"));
    }
    if !ctx.has_scope(names::CALLS_WRITE) {
        return Err(refused("scope"));
    }
    match state.rate_limiter.check(&ctx).await {
        Ok(RateDecision::Allowed) => {}
        Ok(RateDecision::Limited { retry_after_secs }) => {
            return Err(CallingError::RateLimited { retry_after_secs });
        }
        Err(_) => return Err(refused("unavailable")),
    }
    // 4. The user, resolved inside the minting credential's own environment.
    let principal = match state
        .principals
        .resolve(
            &ctx,
            &PrincipalReference::Id(PrincipalId::from_uuid(verified.user.id)),
        )
        .await
    {
        Ok(p) => p,
        Err(PrincipalResolutionError::Unavailable { .. }) => return Err(refused("unavailable")),
        Err(_) => return Err(refused("principal")),
    };
    if principal.kind() != PrincipalKind::ExternalApplicationPrincipal {
        return Err(refused("principal_kind"));
    }

    // The body is read only after authentication (a refused request learns nothing
    // about body validation); it must be a JSON object.
    let bytes = axum::body::to_bytes(Body::new(body), BODY_LIMIT)
        .await
        .map_err(|_| CallingError::Validation("The request body is invalid."))?;
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(CallingError::Validation(
            "The request body must be a JSON object.",
        ));
    }
    let body: ExchangeBody = serde_json::from_slice(&bytes)
        .map_err(|_| CallingError::Validation("The request body is invalid."))?;
    let key = installation_key(&body.installation_id)
        .ok_or(CallingError::Validation("installationId is invalid."))?;

    // 5. The installation's endpoint.
    let scope = CallScope::of_platform(&ctx);
    let new_endpoint = || NewEndpoint {
        key: key.clone(),
        kind: body.kind.endpoint_kind(),
        capabilities: EndpointCapabilities {
            audio: true,
            video: true,
        },
        push_target_ref: None,
    };
    let (endpoint, created) = match state
        .endpoints
        .register(&ctx, &principal, new_endpoint())
        .await
    {
        Ok(registered) => registered,
        Err(CallError::Conflict(_)) => {
            if let Some(previous) = state.endpoints.active_by_key(scope, &key).await? {
                let (_, newly) = state.endpoints.revoke(scope, previous.id).await?;
                if newly {
                    state.app.cancel_endpoint_invitations(previous.id).await?;
                }
                tracing::info!(
                    endpoint_id = %previous.id,
                    "installation endpoint handed over to the signed-in user"
                );
            }
            state
                .endpoints
                .register(&ctx, &principal, new_endpoint())
                .await?
        }
        Err(other) => return Err(other.into()),
    };

    // 6. The signaling credential (never past the user token's expiry).
    let minted = match state.signaling.keys().mint_until(
        scope,
        principal.id(),
        endpoint.id,
        ctx.service_account_id(),
        ctx.credential_id(),
        state.clock.now(),
        verified.expires_at,
    ) {
        Ok(m) => m,
        Err(SignalingCredentialError::Expired) => return Err(refused("token_expiring")),
        Err(SignalingCredentialError::Invalid) => return Err(CallingError::Internal),
    };
    crate::metrics::inc(
        "calling_user_session_exchanges_total",
        &[("outcome", if created { "created" } else { "reused" })],
    );
    tracing::info!(
        endpoint_id = %endpoint.id,
        principal_id = %principal.id(),
        token_id = %verified.token_id,
        "user-session exchange: signaling credential minted"
    );
    Ok(respond(
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        ExchangeView {
            principal_id: principal.id().as_uuid(),
            endpoint_id: endpoint.id.as_uuid(),
            signaling_credential: minted.token.expose_secret().to_string(),
            signaling_credential_expires_at: minted
                .expires_at
                .to_rfc3339_opts(SecondsFormat::Secs, true),
        },
        false,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_ids_map_to_endpoint_keys() {
        assert_eq!(
            installation_key("abcd1234").unwrap().as_str(),
            "inst:abcd1234"
        );
        assert_eq!(
            installation_key("3f2a9c1e-7b3c-4c55-9f2b-1d2e3f4a5b6c")
                .unwrap()
                .as_str(),
            "inst:3f2a9c1e-7b3c-4c55-9f2b-1d2e3f4a5b6c"
        );
        let long = "a".repeat(128);
        let key = installation_key(&long).unwrap();
        assert!(key.as_str().starts_with("inst:h:") && key.as_str().len() <= 128);
        assert_eq!(installation_key(&long), installation_key(&long));
        assert_ne!(installation_key(&long), installation_key(&"b".repeat(128)));
        for bad in ["short", "has space1", "with:colon", "slash/1234", &"a".repeat(129)] {
            assert!(installation_key(bad).is_none(), "{bad}");
        }
        // Leading punctuation is allowed (the key starts with `inst:`).
        assert!(installation_key(".hidden-1").is_some());
    }

    #[test]
    fn only_bearer_user_tokens_are_read() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer cfut_live_a.b.c".parse().unwrap());
        assert_eq!(user_token(&h).as_deref(), Some("cfut_live_a.b.c"));
        h.insert(header::AUTHORIZATION, "Bearer cfsig_a.b.c".parse().unwrap());
        assert!(user_token(&h).is_none());
        h.insert(header::AUTHORIZATION, "Basic cfut_live_a.b.c".parse().unwrap());
        assert!(user_token(&h).is_none());
    }
}
