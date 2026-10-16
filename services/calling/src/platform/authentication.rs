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

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

use super::{
    access::{AuthenticationOutcome, AuthorityError, IdentityAuthority, WireAuthorityMode, WireSuspendedLink},
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
    /// An internal service token was presented to a customer route.
    ServiceCredentialPresented,
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
            .map_err(|e| AuthenticationError::Unavailable {
                summary: match e {
                    AuthorityError::Unavailable(s) | AuthorityError::Refused(s) => s,
                    AuthorityError::NotFound => "not found".to_string(),
                },
            })?;
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
        // The rows every Calling foreign key references must exist (and carry current
        // statuses) before the request can act.
        self.projection
            .apply_chain(&chain)
            .await
            .map_err(|e| AuthenticationError::Unavailable {
                summary: e.to_string(),
            })?;
        let authority_mode = match (chain.authority_mode, chain.suspended_link) {
            (WireAuthorityMode::Full, None) => AuthorityMode::Full,
            (_, Some(WireSuspendedLink::Tenant)) => AuthorityMode::SafetyOnly(SuspendedLink::Tenant),
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
