//! ActorContext — server-resolved identity for the Phase 2 meeting domain (spec §A5).
//!
//! Built only from trusted inputs: a verified access token (registered users) or a
//! server-issued participant credential (guests / invited externals). Request bodies
//! can never supply identity, role, organization or membership.

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts, HeaderName},
};
use sea_orm::EntityTrait;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    middleware::request_id::current_request_id,
    models::users::{self, AuthType, UserRole, UserStatus},
    state::AppState,
};

/// Header carrying a server-issued participant credential (guests, invited externals).
pub const PARTICIPANT_TOKEN_HEADER: HeaderName = HeaderName::from_static("x-participant-token");

/// Optional client hint used to group sessions for support (never identity).
pub const CLIENT_KIND_HEADER: HeaderName = HeaderName::from_static("x-client-kind");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredActor {
    pub user_id: Uuid,
    pub display_name: String,
    /// Present only when the email address is verified.
    pub verified_email: Option<String>,
    pub platform_admin: bool,
    /// Whether the token was checked against the revocation store (Redis). `false`
    /// means the store was unreachable; strict security profiles then fail closed.
    pub revocation_checked: bool,
}

impl RegisteredActor {
    pub fn verified_domain(&self) -> Option<String> {
        self.verified_email
            .as_deref()
            .and_then(|e| e.rsplit_once('@'))
            .map(|(_, d)| d.to_ascii_lowercase())
    }
}

/// A machine caller (Part VI §51): the service account of a Platform Access application
/// environment whose platform credential the API Gateway authenticated and forwarded,
/// re-verified LIVE by Platform Access (`delegated-authority`). Every field comes from
/// the authority chain Platform Access returned — never from the request. Only FULL
/// authority chains become a platform actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformActor {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    /// `SANDBOX`, `TEST` or `PRODUCTION`.
    pub environment_kind: String,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub scopes: std::collections::BTreeSet<String>,
}

impl PlatformActor {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorKind {
    /// Authenticated CF Meet account.
    RegisteredUser(RegisteredActor),
    /// Unauthenticated caller presenting a server-issued participant credential.
    /// Resolved (conference-scoped) by the admission service — never trusted blindly.
    ParticipantCredential(String),
    /// Unauthenticated caller with no credential (new guest / invite holder).
    Anonymous,
    /// Holder of a server-signed legacy v1 guest JWT (`sub` = legacy participant id).
    /// Only meaningful to the legacy v1 join; v2 treats it like `Anonymous`.
    LegacyGuest(Uuid),
    /// Internal scheduler/worker.
    System(&'static str),
    /// Machine caller of a Platform Access application environment (Part VI §51). It
    /// manages ONLY meetings owned by that environment and is never a registered user,
    /// a participant or a meeting member.
    Platform(PlatformActor),
}

/// Trusted, server-resolved actor.
#[derive(Debug, Clone)]
pub struct ActorContext {
    pub kind: ActorKind,
    /// Organization memberships verified through the [`OrganizationDirectory`].
    pub verified_organizations: Vec<Uuid>,
    pub request_id: Option<String>,
    pub client_kind: Option<String>,
}

impl ActorContext {
    pub fn system(name: &'static str) -> Self {
        Self {
            kind: ActorKind::System(name),
            verified_organizations: vec![],
            request_id: current_request_id(),
            client_kind: None,
        }
    }

    pub fn anonymous() -> Self {
        Self {
            kind: ActorKind::Anonymous,
            verified_organizations: vec![],
            request_id: current_request_id(),
            client_kind: None,
        }
    }

    pub fn registered(&self) -> Option<&RegisteredActor> {
        match &self.kind {
            ActorKind::RegisteredUser(r) => Some(r),
            _ => None,
        }
    }

    pub fn user_id(&self) -> Option<Uuid> {
        self.registered().map(|r| r.user_id)
    }

    pub fn platform(&self) -> Option<&PlatformActor> {
        match &self.kind {
            ActorKind::Platform(p) => Some(p),
            _ => None,
        }
    }

    pub fn participant_credential(&self) -> Option<&str> {
        match &self.kind {
            ActorKind::ParticipantCredential(c) => Some(c),
            _ => None,
        }
    }

    /// Require an authenticated CF Meet account.
    pub fn require_registered(&self) -> ApiResult<&RegisteredActor> {
        self.registered().ok_or_else(|| {
            ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "Sign in to perform this action",
            )
        })
    }

    /// Stable reference written to audit/outbox (`user:<id>`, `system:<name>`, …).
    pub fn audit_ref(&self) -> (String, Option<String>) {
        match &self.kind {
            ActorKind::RegisteredUser(r) => ("USER".into(), Some(format!("user:{}", r.user_id))),
            ActorKind::ParticipantCredential(_) => ("PARTICIPANT".into(), None),
            ActorKind::Anonymous => ("ANONYMOUS".into(), None),
            ActorKind::LegacyGuest(p) => (
                "LEGACY_GUEST".into(),
                Some(format!("legacy_participant:{p}")),
            ),
            ActorKind::System(name) => ("SYSTEM".into(), Some(format!("system:{name}"))),
            ActorKind::Platform(p) => (
                "PLATFORM".into(),
                Some(format!("service_account:{}", p.service_account_id)),
            ),
        }
    }
}

/// Verified organization membership source. The meeting service owns no organization
/// directory, so the default reports none; deployments plug in their identity provider.
#[async_trait]
pub trait OrganizationDirectory: Send + Sync {
    async fn organizations_for_user(&self, user_id: Uuid) -> Vec<Uuid>;
}

#[derive(Debug, Default)]
pub struct NoOrganizationDirectory;

#[async_trait]
impl OrganizationDirectory for NoOrganizationDirectory {
    async fn organizations_for_user(&self, _user_id: Uuid) -> Vec<Uuid> {
        vec![]
    }
}

/// Static directory (configuration/tests).
#[derive(Debug, Default)]
pub struct StaticOrganizationDirectory {
    pub memberships: std::collections::HashMap<Uuid, Vec<Uuid>>,
}

#[async_trait]
impl OrganizationDirectory for StaticOrganizationDirectory {
    async fn organizations_for_user(&self, user_id: Uuid) -> Vec<Uuid> {
        self.memberships.get(&user_id).cloned().unwrap_or_default()
    }
}

pub type SharedOrganizationDirectory = Arc<dyn OrganizationDirectory>;

/// Resolve the actor of an HTTP request.
///
/// - `Authorization: Bearer <access token>` → verified registered user (invalid → 401).
/// - Legacy guest JWTs (`role = guest`) are never accepted as v2 identity.
/// - `X-Participant-Token` → participant credential, resolved later in conference scope.
pub async fn resolve_actor(parts: &Parts, state: &AppState) -> ApiResult<ActorContext> {
    let request_id = current_request_id();
    // Part VI §51: a gateway-forwarded machine caller, authenticated by the platform
    // middleware (`middleware::platform_auth`) — the ONLY source of a platform actor.
    if let Some(platform) = parts.extensions.get::<PlatformActor>() {
        return Ok(ActorContext {
            kind: ActorKind::Platform(platform.clone()),
            verified_organizations: vec![],
            request_id,
            client_kind: None,
        });
    }
    // An internal service token is never a user, guest or anonymous identity: without
    // the platform middleware's verified actor it is refused (no fall-through).
    if parts
        .headers
        .contains_key(cf_service_auth::headers::SERVICE_AUTHORIZATION)
    {
        return Err(ApiError::domain(
            ErrorCode::AuthenticationRequired,
            "Authentication failed",
        ));
    }
    let client_kind = parts
        .headers
        .get(&CLIENT_KIND_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(32)
                .collect::<String>()
        })
        .filter(|v| !v.is_empty());

    let bearer = parts
        .headers
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|h| {
            h.strip_prefix("Bearer ")
                .map(str::to_string)
                .ok_or_else(|| {
                    ApiError::domain(
                        ErrorCode::AuthenticationRequired,
                        "Invalid authorization format",
                    )
                })
        })
        .transpose()?;

    if let Some(token) = bearer {
        let claims = state.jwt_service.verify_token(&token).map_err(|_| {
            ApiError::domain(
                ErrorCode::AuthenticationRequired,
                "Invalid or expired token",
            )
        })?;
        // Per-token revocation (logout) lives ONLY in Redis (`TokenBlacklistService`).
        // Account deactivation is authoritative in PostgreSQL and checked below on every
        // request. If Redis is unreachable the token is honoured but marked
        // `revocation_checked = false`; strict security profiles fail closed on that.
        let revocation_checked = match state.token_blacklist.is_blacklisted(&token).await {
            Ok(true) => {
                return Err(ApiError::domain(
                    ErrorCode::AuthenticationRequired,
                    "Token has been revoked",
                ))
            }
            Ok(false) => true,
            Err(e) => {
                tracing::warn!("token revocation check unavailable (Redis): {e}");
                false
            }
        };
        if claims.role == "guest" {
            // Legacy v1 guest JWT: identifies a legacy guest participant only.
            if let Ok(participant_id) = Uuid::parse_str(&claims.sub) {
                return Ok(ActorContext {
                    kind: ActorKind::LegacyGuest(participant_id),
                    verified_organizations: vec![],
                    request_id,
                    client_kind,
                });
            }
        } else {
            let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
                ApiError::domain(ErrorCode::AuthenticationRequired, "Invalid token subject")
            })?;
            let user = users::Entity::find_by_id(user_id)
                .one(&state.db)
                .await?
                .ok_or_else(|| {
                    ApiError::domain(ErrorCode::AuthenticationRequired, "Account not found")
                })?;
            if user.status == UserStatus::Inactive || user.role == UserRole::Guest {
                return Err(ApiError::domain(
                    ErrorCode::AuthenticationRequired,
                    "Account is not active",
                ));
            }
            let email_verified = user.auth_type == AuthType::Google
                || (user.status == UserStatus::Active && state.require_email_verification);
            let verified_organizations = state
                .organization_directory
                .organizations_for_user(user.id)
                .await;
            return Ok(ActorContext {
                kind: ActorKind::RegisteredUser(RegisteredActor {
                    user_id: user.id,
                    display_name: user.username.clone(),
                    verified_email: email_verified.then(|| user.email.to_ascii_lowercase()),
                    platform_admin: user.role == UserRole::Admin,
                    revocation_checked,
                }),
                verified_organizations,
                request_id,
                client_kind,
            });
        }
    }

    if let Some(credential) = parts
        .headers
        .get(&PARTICIPANT_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
    {
        return Ok(ActorContext {
            kind: ActorKind::ParticipantCredential(credential.to_string()),
            verified_organizations: vec![],
            request_id,
            client_kind,
        });
    }

    Ok(ActorContext {
        kind: ActorKind::Anonymous,
        verified_organizations: vec![],
        request_id,
        client_kind,
    })
}

#[async_trait]
impl FromRequestParts<AppState> for ActorContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        resolve_actor(parts, state).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_domain_requires_verified_email() {
        let mut actor = RegisteredActor {
            user_id: Uuid::new_v4(),
            display_name: "a".into(),
            verified_email: Some("Alice@Example.COM".into()),
            platform_admin: false,
            revocation_checked: true,
        };
        assert_eq!(actor.verified_domain().as_deref(), Some("example.com"));
        actor.verified_email = None;
        assert_eq!(actor.verified_domain(), None);
    }
}
