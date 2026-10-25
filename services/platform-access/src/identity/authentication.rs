//! Credential authentication: the single place a presented customer API credential is
//! verified on the platform.
//!
//! One indexed query loads the presented credential's full authority chain (credential,
//! service account, environment, application, tenant, granted scopes with their
//! application grants, authorized-principal restriction). Authentication succeeds only if:
//!
//! ```text
//! the secret verifies (constant time, pepper of the persisted version)
//! AND the presented environment tag matches the persisted environment
//! AND credential ACTIVE, not revoked, not expired, not before not_before,
//!     not past its rotation overlap
//! AND service account ACTIVE
//! ```
//!
//! A SUSPENDED environment, application or tenant does not reject the credential: the
//! chain is returned with [`AuthorityMode::SafetyOnly`] and the consuming service admits
//! only its safety allow-list (Part H §72 / §74).
//!
//! There is no cache: suspension, revocation and rotation take effect on the next
//! request. Callers only ever learn a bounded [`AuthFailureReason`] (for their operator
//! logs) — never the secret, the verifier or which part of a secret was wrong.
//!
//! The verifier message, format and keyring are exactly Calling's Part III ones, so every
//! credential issued before the extraction keeps verifying unchanged.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    credential_format::{self, PresentedCredential},
    credentials::CredentialEffectiveState,
    error::summarize_db_error,
    scopes::{GrantBasis, PersistedGrant, ScopeRegistry},
    snapshots::{
        from_row_json, ApplicationSnapshot, CredentialSnapshot, EnvironmentSnapshot,
        ServiceAccountSnapshot, TenantSnapshot,
    },
    verifier::{VerifierKeyring, VERIFIER_ALGORITHM},
};
use crate::{
    domain::{
        ApplicationStatus, EnvironmentKind, EnvironmentStatus, ServiceAccountStatus, SharedClock,
        TenantStatus,
    },
    models::api_credentials::Verifier,
};

/// Why authentication was refused. Returned to the (trusted, internal) caller for its
/// operator logs; customers only ever see a normalized authentication failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFailureReason {
    MalformedCredential,
    UnknownCredential,
    SecretMismatch,
    EnvironmentTagMismatch,
    UnsupportedVerifier,
    CredentialRevoked,
    CredentialNotYetValid,
    CredentialExpired,
    CredentialSuperseded,
    /// Reversibly suspended (Part VI §54).
    CredentialSuspended,
    ServiceAccountInactive,
    CorruptAuthorityChain,
}

impl AuthFailureReason {
    /// Every reason (the contract's `AuthFailureReason` enumeration).
    pub const ALL: &'static [AuthFailureReason] = &[
        Self::MalformedCredential,
        Self::UnknownCredential,
        Self::SecretMismatch,
        Self::EnvironmentTagMismatch,
        Self::UnsupportedVerifier,
        Self::CredentialRevoked,
        Self::CredentialNotYetValid,
        Self::CredentialExpired,
        Self::CredentialSuperseded,
        Self::CredentialSuspended,
        Self::ServiceAccountInactive,
        Self::CorruptAuthorityChain,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MalformedCredential => "malformed_credential",
            Self::UnknownCredential => "unknown_credential",
            Self::SecretMismatch => "secret_mismatch",
            Self::EnvironmentTagMismatch => "environment_tag_mismatch",
            Self::UnsupportedVerifier => "unsupported_verifier",
            Self::CredentialRevoked => "credential_revoked",
            Self::CredentialNotYetValid => "credential_not_yet_valid",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialSuperseded => "credential_superseded",
            Self::CredentialSuspended => "credential_suspended",
            Self::ServiceAccountInactive => "service_account_inactive",
            Self::CorruptAuthorityChain => "corrupt_authority_chain",
        }
    }
}

/// Which link of the authority chain is SUSPENDED (Part H §72 / §74).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuspendedLink {
    Tenant,
    Application,
    Environment,
}

/// What the authenticated credential may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthorityMode {
    /// Every link of the chain is ACTIVE; the scopes decide.
    Full,
    /// The credential and service account are valid but the tenant, application or
    /// environment is SUSPENDED: only the consumer's safety allow-list.
    SafetyOnly,
}

/// The trusted authority of an authenticated credential: snapshots of every link, the
/// effective scopes and the authorized-principal restriction. Contract
/// `cfmeet.platform-access.identity` `AuthorityChain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityChain {
    pub authority_mode: AuthorityMode,
    pub suspended_link: Option<SuspendedLink>,
    pub tenant: TenantSnapshot,
    pub application: ApplicationSnapshot,
    pub environment: EnvironmentSnapshot,
    pub service_account: ServiceAccountSnapshot,
    pub credential: CredentialSnapshot,
    pub effective_state: CredentialEffectiveState,
    /// Effective scopes, sorted.
    pub scopes: Vec<String>,
    /// `Some`: the credential may act only for these principals (sorted).
    pub principal_restriction: Option<Vec<Uuid>>,
}

/// Failed-authentication throttling outcome (§109): the caller answers 429 with
/// `Retry-After` instead of 401; `threshold_crossed` is true exactly once per window so
/// the caller can raise one deduplicated guessing incident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Throttle {
    pub retry_after_seconds: u64,
    pub threshold_crossed: bool,
}

#[derive(Debug)]
pub enum AuthenticationError {
    /// The credential does not authenticate.
    Rejected {
        reason: AuthFailureReason,
        /// Non-secret lookup id, when the value was well-formed (for operator logs).
        public_id: Option<String>,
    },
    /// The authority store is unreachable; the request is refused (fail closed).
    Unavailable { summary: String },
}

impl AuthenticationError {
    fn rejected(reason: AuthFailureReason, presented: Option<&PresentedCredential>) -> Self {
        Self::Rejected {
            reason,
            public_id: presented.map(|p| p.public_id.clone()),
        }
    }
}

/// [`AUTHORITY_CHAIN_SQL`] keyed by the credential id instead of its public id.
static AUTHORITY_CHAIN_BY_ID_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    AUTHORITY_CHAIN_SQL.replace("WHERE c.public_id = $1", "WHERE c.id = $1")
});

const AUTHORITY_CHAIN_SQL: &str = r#"
SELECT
    c.verifier                 AS verifier,
    c.verifier_algorithm       AS verifier_algorithm,
    c.verifier_key_version     AS verifier_key_version,
    to_jsonb(c) - 'verifier' - 'verifier_algorithm' - 'verifier_key_version' AS credential,
    to_jsonb(sa)               AS service_account,
    to_jsonb(e)                AS environment,
    to_jsonb(a)                AS application,
    to_jsonb(t)                AS tenant,
    COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
                   'scope', cs.scope,
                   'basis', aps.grant_basis,
                   'environmentAllowed',
                       aps.allowed_environment_kinds IS NULL OR e.kind = ANY (aps.allowed_environment_kinds))
               ORDER BY cs.scope)
        FROM platform_api_credential_scopes cs
        JOIN platform_application_scopes aps
          ON aps.application_id = cs.application_id AND aps.scope = cs.scope
        WHERE cs.credential_id = c.id
    ), '[]'::jsonb)            AS grants,
    (SELECT array_agg(r.principal_id ORDER BY r.principal_id)
       FROM platform_credential_principal_restrictions r
      WHERE r.credential_id = c.id) AS restricted_principals,
    EXISTS (SELECT 1 FROM platform_credential_suspensions s
             WHERE s.credential_id = c.id) AS suspended
FROM platform_api_credentials c
JOIN platform_service_accounts sa
  ON sa.id = c.service_account_id
 AND sa.tenant_id = c.tenant_id
 AND sa.application_id = c.application_id
 AND sa.environment_id = c.environment_id
JOIN platform_environments e
  ON e.id = c.environment_id
 AND e.tenant_id = c.tenant_id
 AND e.application_id = c.application_id
JOIN platform_applications a
  ON a.id = c.application_id
 AND a.tenant_id = c.tenant_id
JOIN platform_tenants t
  ON t.id = c.tenant_id
WHERE c.public_id = $1
"#;

#[derive(FromQueryResult)]
struct AuthorityRow {
    verifier: Verifier,
    verifier_algorithm: String,
    verifier_key_version: i32,
    credential: Value,
    service_account: Value,
    environment: Value,
    application: Value,
    tenant: Value,
    grants: Value,
    restricted_principals: Option<Vec<Uuid>>,
    suspended: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantRow {
    scope: String,
    basis: String,
    environment_allowed: bool,
}

/// Bounded failed-authentication windows (§109): more than `threshold` FAILED
/// authentications of one presented credential key id within one `window` are throttled.
/// A CORRECT secret is always accepted (the window never locks a credential out). Unknown
/// key ids share one bucket, so guessing ids never grows the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthFailurePolicy {
    pub threshold: i64,
    pub window: chrono::Duration,
}

impl Default for AuthFailurePolicy {
    fn default() -> Self {
        Self {
            threshold: 20,
            window: chrono::Duration::seconds(300),
        }
    }
}

/// The failure bucket of every presented key id that names no credential.
const UNKNOWN_KEY_BUCKET: &str = "unknown";

/// Verifies presented credentials against the persisted authority chain.
pub struct CredentialAuthenticator {
    db: DatabaseConnection,
    keyring: Arc<VerifierKeyring>,
    registry: Arc<ScopeRegistry>,
    clock: SharedClock,
    failures: AuthFailurePolicy,
}

impl CredentialAuthenticator {
    pub fn new(
        db: DatabaseConnection,
        keyring: Arc<VerifierKeyring>,
        registry: Arc<ScopeRegistry>,
        clock: SharedClock,
    ) -> Self {
        Self {
            db,
            keyring,
            registry,
            clock,
            failures: AuthFailurePolicy::default(),
        }
    }

    /// Uses `policy` for the failed-authentication windows.
    pub fn with_failure_policy(mut self, policy: AuthFailurePolicy) -> Self {
        self.failures = policy;
        self
    }

    /// Counts one FAILED authentication of a presented credential key id (a secret that
    /// did not verify, or a key id that names no credential). Failures after the secret
    /// verified (revoked, expired, …) and malformed values are not guessing and are not
    /// counted.
    pub async fn note_failure(
        &self,
        reason: AuthFailureReason,
        public_id: Option<&str>,
    ) -> Result<Option<Throttle>, DbErr> {
        let key = match (reason, public_id) {
            (AuthFailureReason::UnknownCredential, _) => UNKNOWN_KEY_BUCKET,
            (
                AuthFailureReason::SecretMismatch | AuthFailureReason::UnsupportedVerifier,
                Some(id),
            ) if id.len() == 24
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                id
            }
            _ => return Ok(None),
        };
        let now = self.clock.now();
        let window = self.failures.window.num_seconds().max(1);
        let start_secs = now.timestamp().div_euclid(window) * window;
        let window_start = DateTime::<Utc>::from_timestamp(start_secs, 0).unwrap_or(now);
        let failures: i64 = self
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO platform_auth_failure_windows (subject_key, window_start, failures) \
                 VALUES ($1, $2, 1) \
                 ON CONFLICT (subject_key, window_start) \
                 DO UPDATE SET failures = platform_auth_failure_windows.failures + 1 \
                 RETURNING failures",
                [key.into(), window_start.into()],
            ))
            .await?
            .map(|r| r.try_get_by_index::<i64>(0))
            .transpose()?
            .unwrap_or(0);
        if failures <= self.failures.threshold {
            return Ok(None);
        }
        let threshold_crossed = failures == self.failures.threshold + 1;
        if threshold_crossed {
            crate::metrics::inc(
                "platform_access_credential_guessing_total",
                &[(
                    "bucket",
                    if key == UNKNOWN_KEY_BUCKET {
                        "unknown"
                    } else {
                        "credential"
                    },
                )],
            );
            tracing::warn!(
                public_id = if key == UNKNOWN_KEY_BUCKET { "" } else { key },
                threshold = self.failures.threshold,
                "repeated failed authentication: credential guessing suspected"
            );
        }
        let retry = (start_secs + window - now.timestamp()).max(1) as u64;
        Ok(Some(Throttle {
            retry_after_seconds: retry,
            threshold_crossed,
        }))
    }

    /// Deletes failed-authentication windows that started before `before`.
    pub async fn prune_failure_windows(&self, before: DateTime<Utc>) -> Result<u64, DbErr> {
        Ok(self
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM platform_auth_failure_windows WHERE window_start < $1",
                [before.into()],
            ))
            .await?
            .rows_affected())
    }

    /// Authenticates a presented credential string (the bearer token).
    pub async fn authenticate(
        &self,
        presented: &str,
    ) -> Result<AuthorityChain, AuthenticationError> {
        let Some(presented) = credential_format::parse(presented) else {
            return Err(AuthenticationError::rejected(
                AuthFailureReason::MalformedCredential,
                None,
            ));
        };
        let row = self.load(&presented.public_id).await.map_err(|e| {
            AuthenticationError::Unavailable {
                summary: summarize_db_error(&e),
            }
        })?;
        let message = credential_format::verifier_message(
            presented.tag_kind,
            &presented.public_id,
            &presented.secret,
        );
        let Some(row) = row else {
            // Equalise work with the known-credential path.
            let _ = self
                .keyring
                .verify(self.keyring.current_version(), &message, &[0u8; 32]);
            return Err(AuthenticationError::rejected(
                AuthFailureReason::UnknownCredential,
                Some(&presented),
            ));
        };

        if row.verifier_algorithm != VERIFIER_ALGORITHM
            || !self.keyring.has_version(row.verifier_key_version)
        {
            let _ = self
                .keyring
                .verify(self.keyring.current_version(), &message, &row.verifier.0);
            return Err(AuthenticationError::rejected(
                AuthFailureReason::UnsupportedVerifier,
                Some(&presented),
            ));
        }
        if !self
            .keyring
            .verify(row.verifier_key_version, &message, &row.verifier.0)
        {
            return Err(AuthenticationError::rejected(
                AuthFailureReason::SecretMismatch,
                Some(&presented),
            ));
        }

        evaluate(row, presented.tag_kind, self.clock.now(), &self.registry)
            .map_err(|reason| AuthenticationError::rejected(reason, Some(&presented)))
    }

    /// The CURRENT authority chain of a credential identified by its Platform Access id,
    /// for a receiver re-verifying the credential the API Gateway authenticated (Part VI
    /// §51). No secret is involved: the caller proves only that a trusted gateway already
    /// verified the secret; revocation, expiry, supersession, suspension and effective
    /// scopes are evaluated here, now, exactly as at authentication.
    pub async fn authority_of(
        &self,
        credential_id: uuid::Uuid,
    ) -> Result<AuthorityChain, AuthenticationError> {
        let row = AuthorityRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            AUTHORITY_CHAIN_BY_ID_SQL.as_str(),
            [credential_id.into()],
        ))
        .one(&self.db)
        .await
        .map_err(|e| AuthenticationError::Unavailable {
            summary: summarize_db_error(&e),
        })?
        .ok_or_else(|| AuthenticationError::rejected(AuthFailureReason::UnknownCredential, None))?;
        let kind = row
            .environment
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .and_then(EnvironmentKind::parse)
            .ok_or_else(|| {
                AuthenticationError::rejected(AuthFailureReason::CorruptAuthorityChain, None)
            })?;
        evaluate(row, kind, self.clock.now(), &self.registry)
            .map_err(|reason| AuthenticationError::rejected(reason, None))
    }

    async fn load(&self, public_id: &str) -> Result<Option<AuthorityRow>, DbErr> {
        AuthorityRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            AUTHORITY_CHAIN_SQL,
            [public_id.into()],
        ))
        .one(&self.db)
        .await
    }
}

/// The authority-chain decision, after the secret has verified.
fn evaluate(
    row: AuthorityRow,
    presented_kind: EnvironmentKind,
    now: DateTime<Utc>,
    registry: &ScopeRegistry,
) -> Result<AuthorityChain, AuthFailureReason> {
    use AuthFailureReason as R;
    let corrupt = |_| R::CorruptAuthorityChain;
    let credential: CredentialSnapshot = from_row_json(row.credential).map_err(corrupt)?;
    let service_account: ServiceAccountSnapshot =
        from_row_json(row.service_account).map_err(corrupt)?;
    let environment: EnvironmentSnapshot = from_row_json(row.environment).map_err(corrupt)?;
    let application: ApplicationSnapshot = from_row_json(row.application).map_err(corrupt)?;
    let tenant: TenantSnapshot = from_row_json(row.tenant).map_err(corrupt)?;
    let grants: Vec<GrantRow> = serde_json::from_value(row.grants).map_err(corrupt)?;

    // The tag is bound into the verifier message too; this is an explicit second check.
    if environment.kind != presented_kind {
        return Err(R::EnvironmentTagMismatch);
    }

    let effective_state = CredentialEffectiveState::evaluate(
        credential.status,
        credential.not_before,
        credential.expires_at,
        credential.supersession_valid_until,
        now,
    );
    match effective_state {
        CredentialEffectiveState::Active | CredentialEffectiveState::RotationOverlap => {}
        CredentialEffectiveState::Revoked => return Err(R::CredentialRevoked),
        CredentialEffectiveState::NotYetValid => return Err(R::CredentialNotYetValid),
        CredentialEffectiveState::Expired => return Err(R::CredentialExpired),
        CredentialEffectiveState::Superseded => return Err(R::CredentialSuperseded),
        // `evaluate` never derives it from persisted status; suspension is below.
        CredentialEffectiveState::Suspended => return Err(R::CredentialSuspended),
    }
    if row.suspended {
        return Err(R::CredentialSuspended);
    }
    if service_account.status != ServiceAccountStatus::Active {
        return Err(R::ServiceAccountInactive);
    }
    // A SUSPENDED scope keeps a safety-only authority (Part H §72): the outermost
    // suspended link is reported.
    let suspended_link = if tenant.status != TenantStatus::Active {
        Some(SuspendedLink::Tenant)
    } else if application.status != ApplicationStatus::Active {
        Some(SuspendedLink::Application)
    } else if environment.status != EnvironmentStatus::Active {
        Some(SuspendedLink::Environment)
    } else {
        None
    };
    let scopes = registry
        .effective(
            grants.into_iter().map(|g| PersistedGrant {
                scope: g.scope,
                // An unknown basis is never privileged (fail closed).
                grant_basis: GrantBasis::parse(&g.basis).unwrap_or(GrantBasis::Standard),
                environment_allowed: g.environment_allowed,
            }),
            tenant.classification,
        )
        .names();
    // Defence in depth: never report live telecom outside PRODUCTION.
    let mut environment = environment;
    environment.live_telecom_allowed &= environment.kind.may_allow_live_telecom();
    Ok(AuthorityChain {
        authority_mode: if suspended_link.is_some() {
            AuthorityMode::SafetyOnly
        } else {
            AuthorityMode::Full
        },
        suspended_link,
        tenant,
        application,
        environment,
        service_account,
        credential,
        effective_state,
        scopes,
        principal_restriction: row.restricted_principals,
    })
}
