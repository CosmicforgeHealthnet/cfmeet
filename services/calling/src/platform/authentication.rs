//! Customer credential authentication — delegated to Platform Access (ADR 0032).
//!
//! Calling no longer holds any verifier, pepper or credential secret logic. A presented
//! bearer credential is sent, over the authenticated internal contract, to Platform
//! Access (`POST /internal/v1/credentials/authenticate`), which verifies it and returns
//! the trusted authority chain:
//!
//! ```text
//! credential ─▶ Platform Access ─▶ AUTHENTICATED { tenant, application, environment,
//!                                                   service account, credential,
//!                                                   effective scopes, authority mode }
//!                                ─▶ REJECTED { reason, throttle? }
//! ```
//!
//! On success the chain is applied to Calling's identity projection (the rows Calling's
//! foreign keys reference) and turned into a [`PlatformAuthContext`] — every field from
//! the authority, never from the request. Customers only ever see the same normalized
//! responses as before the extraction: 401 for a rejected credential, 429 with
//! `Retry-After` while a key id is throttled (§109), 503 when the authority is
//! unreachable (fail closed). Suspension, revocation and rotation take effect on the next
//! request (Platform Access keeps no cache).
//!
//! A second path serves requests forwarded by the public API Gateway (Part VI §51,
//! [`CredentialAuthenticator::authenticate_delegated`]). The gateway verified the bearer
//! credential itself and forwards the request without it, naming the credential (id
//! only, never a secret) in the delegated context of its signed service token. Calling
//! asks Platform Access (`POST /internal/v1/credentials/delegated-authority`, the
//! delegated context attached to Calling's own outbound token) for that credential's
//! LIVE authority chain, so a credential revoked or suspended after the gateway's check
//! is refused here too:
//!
//! ```text
//! delegated context ─▶ Platform Access ─▶ AUTHENTICATED { same chain as above }
//!  (credential id)                     ─▶ REJECTED { reason }   (no throttle: no secret)
//! ```
//!
//! Both paths share one chain handler (projection, authority mode, scopes), so a
//! forwarded request is scoped exactly like a directly authenticated one.

use std::sync::Arc;

use cf_service_auth::DelegatedPlatformContext;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

use super::{
    access::{
        AuthenticationOutcome, AuthorityChain, AuthorityError, IdentityAuthority,
        WireAuthorityMode, WireSuspendedLink,
    },
    context::{AuthorityMode, PlatformAuthContext, SuspendedLink, TrustedAuthority},
    projection::IdentityProjection,
    scopes::ScopeSet,
};
use crate::domain::{
    ApplicationId, CredentialId, EnvironmentId, RequestContext, ServiceAccountId, SharedClock,
    TenantId,
};

/// Why authentication was refused. Logged for operators; never returned to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailureReason {
    MissingCredential,
    /// An internal service token was presented together with a customer credential.
    ServiceCredentialPresented,
    /// A service token on a customer route did not verify against Calling's trust bundle.
    ServiceTokenInvalid,
    /// A verified service token whose issuer is not granted `calling-request.forward`.
    ForwardNotPermitted,
    /// A gateway-forwarded request without a delegated context naming the credential.
    DelegatedCredentialMissing,
    MalformedCredential,
    UnknownCredential,
    SecretMismatch,
    EnvironmentTagMismatch,
    UnsupportedVerifier,
    CredentialRevoked,
    CredentialNotYetValid,
    CredentialExpired,
    CredentialSuperseded,
    ServiceAccountInactive,
    /// The scope is SUSPENDED and the request is not a safety operation (Part H §72).
    ScopeSuspended,
    CorruptAuthorityChain,
}

impl AuthFailureReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingCredential => "missing_credential",
            Self::ServiceCredentialPresented => "service_credential_presented",
            Self::ServiceTokenInvalid => "service_token_invalid",
            Self::ForwardNotPermitted => "forward_not_permitted",
            Self::DelegatedCredentialMissing => "delegated_credential_missing",
            Self::MalformedCredential => "malformed_credential",
            Self::UnknownCredential => "unknown_credential",
            Self::SecretMismatch => "secret_mismatch",
            Self::EnvironmentTagMismatch => "environment_tag_mismatch",
            Self::UnsupportedVerifier => "unsupported_verifier",
            Self::CredentialRevoked => "credential_revoked",
            Self::CredentialNotYetValid => "credential_not_yet_valid",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialSuperseded => "credential_superseded",
            Self::ServiceAccountInactive => "service_account_inactive",
            Self::ScopeSuspended => "scope_suspended",
            Self::CorruptAuthorityChain => "corrupt_authority_chain",
        }
    }

    /// The bounded reason code of the authority's answer (an unknown code is a corrupt
    /// answer — it can only ever refuse).
    pub fn from_authority(code: &str) -> Self {
        match code {
            "malformed_credential" => Self::MalformedCredential,
            "unknown_credential" => Self::UnknownCredential,
            "secret_mismatch" => Self::SecretMismatch,
            "environment_tag_mismatch" => Self::EnvironmentTagMismatch,
            "unsupported_verifier" => Self::UnsupportedVerifier,
            "credential_revoked" => Self::CredentialRevoked,
            "credential_not_yet_valid" => Self::CredentialNotYetValid,
            "credential_expired" => Self::CredentialExpired,
            "credential_superseded" => Self::CredentialSuperseded,
            "service_account_inactive" => Self::ServiceAccountInactive,
            _ => Self::CorruptAuthorityChain,
        }
    }
}

#[derive(Debug)]
pub enum AuthenticationError {
    /// The credential does not authenticate. Clients see a normalized 401.
    Rejected {
        reason: AuthFailureReason,
        /// Non-secret lookup id, when the value was well-formed (for operator logs).
        public_id: Option<String>,
    },
    /// The key id is above its failed-authentication window: 429 with Retry-After.
    Throttled {
        reason: AuthFailureReason,
        public_id: Option<String>,
        retry_after_secs: u64,
    },
    /// The authority (or Calling's projection) is unreachable: refused, fail closed.
    Unavailable { summary: String },
}

/// Every failure to reach the authority (or a refused authority call) fails closed.
fn unavailable(error: AuthorityError) -> AuthenticationError {
    AuthenticationError::Unavailable {
        summary: match error {
            AuthorityError::Unavailable(s) | AuthorityError::Refused(s) => s,
            AuthorityError::NotFound => "not found".to_string(),
        },
    }
}

/// The failure bucket of every presented key id that names no credential.
const UNKNOWN_KEY_BUCKET: &str = "unknown";

/// Authenticates presented credentials through Platform Access.
pub struct CredentialAuthenticator {
    db: DatabaseConnection,
    authority: Arc<dyn IdentityAuthority>,
    projection: Arc<IdentityProjection>,
    clock: SharedClock,
}

impl CredentialAuthenticator {
    pub fn new(
        db: DatabaseConnection,
        authority: Arc<dyn IdentityAuthority>,
        projection: Arc<IdentityProjection>,
        clock: SharedClock,
    ) -> Self {
        Self {
            db,
            authority,
            projection,
            clock,
        }
    }

    /// Authenticates a presented credential string (the bearer token).
    pub async fn authenticate(
        &self,
        presented: &str,
        request: RequestContext,
    ) -> Result<PlatformAuthContext, AuthenticationError> {
        let outcome = self
            .authority
            .authenticate(presented, &request.correlation_id)
            .await
            .map_err(unavailable)?;
        let chain = match outcome {
            AuthenticationOutcome::Authenticated(chain) => chain,
            AuthenticationOutcome::Rejected {
                reason,
                public_id,
                throttle,
            } => {
                let reason = AuthFailureReason::from_authority(&reason);
                return Err(match throttle {
                    Some(t) => {
                        if t.threshold_crossed {
                            self.guessing_incident(public_id.as_deref()).await;
                        }
                        AuthenticationError::Throttled {
                            reason,
                            public_id,
                            retry_after_secs: t.retry_after_seconds.max(1),
                        }
                    }
                    None => AuthenticationError::Rejected { reason, public_id },
                });
            }
        };
        self.trusted_context(&chain, request).await
    }

    /// Authenticates a request the API Gateway forwarded (§51): the gateway already
    /// verified the customer's bearer credential and named it in its signed delegated
    /// context (`initiating_credential_id`). The caller has verified that service token
    /// (issuer granted `calling-request.forward`). Nothing from the delegated context is
    /// trusted as authority: Platform Access re-fetches the credential's LIVE authority
    /// chain (so revocation, expiry and suspension apply on the very next request) and the
    /// chain is handled exactly like a directly presented credential's. No secret is
    /// presented here, so no failed-authentication throttle applies: a REJECTED answer is
    /// the normalized 401, an unreachable authority 503.
    pub async fn authenticate_delegated(
        &self,
        delegated: &DelegatedPlatformContext,
        request: RequestContext,
    ) -> Result<PlatformAuthContext, AuthenticationError> {
        let Some(credential_id) = delegated.initiating_credential_id() else {
            return Err(AuthenticationError::Rejected {
                reason: AuthFailureReason::DelegatedCredentialMissing,
                public_id: None,
            });
        };
        let outcome = self
            .authority
            .delegated_authority(delegated, &request.correlation_id)
            .await
            .map_err(unavailable)?;
        let chain = match outcome {
            AuthenticationOutcome::Authenticated(chain) => chain,
            AuthenticationOutcome::Rejected {
                reason, public_id, ..
            } => {
                return Err(AuthenticationError::Rejected {
                    reason: AuthFailureReason::from_authority(&reason),
                    public_id,
                })
            }
        };
        // Platform Access already refuses a chain outside the delegated scope; an answer
        // naming another credential or scope is corrupt and can only refuse.
        let same_authority = chain.credential.id == credential_id
            && chain.tenant.id == delegated.tenant_id()
            && chain.application.id == delegated.application_id()
            && chain.environment.id == delegated.application_environment_id()
            && chain.service_account.id == delegated.initiating_service_account_id();
        if !same_authority {
            return Err(AuthenticationError::Rejected {
                reason: AuthFailureReason::CorruptAuthorityChain,
                public_id: Some(chain.credential.public_id.clone()),
            });
        }
        self.trusted_context(&chain, request).await
    }

    /// The single path from an authority chain to a [`PlatformAuthContext`]: projection,
    /// authority mode, scope set — shared by every authentication path.
    async fn trusted_context(
        &self,
        chain: &AuthorityChain,
        request: RequestContext,
    ) -> Result<PlatformAuthContext, AuthenticationError> {
        // The rows every Calling foreign key references must exist (and carry current
        // statuses) before the request can act.
        self.projection
            .apply_chain(chain)
            .await
            .map_err(|e| AuthenticationError::Unavailable {
                summary: e.to_string(),
            })?;
        let authority_mode = match (chain.authority_mode, chain.suspended_link) {
            (WireAuthorityMode::Full, None) => AuthorityMode::Full,
            (_, Some(WireSuspendedLink::Tenant)) => {
                AuthorityMode::SafetyOnly(SuspendedLink::Tenant)
            }
            (_, Some(WireSuspendedLink::Application)) => {
                AuthorityMode::SafetyOnly(SuspendedLink::Application)
            }
            (_, Some(WireSuspendedLink::Environment)) => {
                AuthorityMode::SafetyOnly(SuspendedLink::Environment)
            }
            // A safety-only authority without its link is a corrupt answer: refuse.
            (WireAuthorityMode::SafetyOnly, None) => {
                return Err(AuthenticationError::Rejected {
                    reason: AuthFailureReason::CorruptAuthorityChain,
                    public_id: Some(chain.credential.public_id.clone()),
                })
            }
        };
        Ok(PlatformAuthContext::from_trusted(
            TrustedAuthority {
                authority_mode,
                tenant_id: TenantId::from_uuid(chain.tenant.id),
                tenant_classification: chain.tenant.classification,
                application_id: ApplicationId::from_uuid(chain.application.id),
                environment_id: EnvironmentId::from_uuid(chain.environment.id),
                environment_kind: chain.environment.kind,
                live_telecom_allowed: chain.environment.live_telecom_allowed,
                service_account_id: ServiceAccountId::from_uuid(chain.service_account.id),
                credential_id: CredentialId::from_uuid(chain.credential.id),
                scopes: ScopeSet::from_authority(chain.scopes.iter().cloned()),
            },
            request,
        ))
    }

    /// One deduplicated CREDENTIAL_GUESSING incident when Platform Access reports that a
    /// key id crossed its failed-authentication threshold (§109).
    async fn guessing_incident(&self, public_id: Option<&str>) {
        use crate::calls::app::incidents::{
            self, Incident, IncidentCategory, IncidentResource, IncidentSeverity,
        };
        let now: DateTime<Utc> = self.clock.now();
        let key = public_id
            .filter(|id| {
                id.len() == 24
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .unwrap_or(UNKNOWN_KEY_BUCKET);
        let incident = if key == UNKNOWN_KEY_BUCKET {
            Some(Incident::platform(
                IncidentCategory::CredentialGuessing,
                IncidentSeverity::High,
                "unknown_key_ids",
            ))
        } else {
            let row = self
                .db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id, tenant_id, application_id, environment_id \
                     FROM platform_api_credentials WHERE public_id = $1",
                    [key.into()],
                ))
                .await
                .ok()
                .flatten();
            row.and_then(|r| {
                let ids: (uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid) = (
                    r.try_get_by_index(0).ok()?,
                    r.try_get_by_index(1).ok()?,
                    r.try_get_by_index(2).ok()?,
                    r.try_get_by_index(3).ok()?,
                );
                Some(Incident::new(
                    IncidentCategory::CredentialGuessing,
                    IncidentSeverity::High,
                    crate::calls::command::CallScope::from_parts(
                        TenantId::from_uuid(ids.1),
                        ApplicationId::from_uuid(ids.2),
                        EnvironmentId::from_uuid(ids.3),
                    ),
                    IncidentResource::Credential,
                    ids.0,
                ))
            })
            // A key id Calling never projected is reported as an unknown-id signal.
            .or_else(|| {
                Some(Incident::platform(
                    IncidentCategory::CredentialGuessing,
                    IncidentSeverity::High,
                    "unknown_key_ids",
                ))
            })
        };
        if let Some(incident) = incident {
            incidents::raise(&self.db, &incident, now).await;
        }
    }
}
