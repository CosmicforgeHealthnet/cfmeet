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
    /// ADR 0064: present only for an OAuth app token (`OAUTH_ACCESS` | `OAUTH_BOT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_kind: Option<OAuthCredentialKind>,
    /// ADR 0064: the acting app (its client id), OAuth tokens only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<Uuid>,
    /// ADR 0064: the grant (installation) the OAuth token belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<Uuid>,
}

/// The kind of an OAuth app credential (ADR 0064).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OAuthCredentialKind {
    #[serde(rename = "OAUTH_ACCESS")]
    Access,
    #[serde(rename = "OAUTH_BOT")]
    Bot,
}

impl OAuthCredentialKind {
    fn from_token_kind(kind: &str) -> Option<Self> {
        match kind {
            "ACCESS" => Some(Self::Access),
            "BOT" => Some(Self::Bot),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Access => "OAUTH_ACCESS",
            Self::Bot => "OAUTH_BOT",
        }
    }
}

/// Which credential family a verification expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A `cfmk_` API credential (never an OAuth token row).
    ApiKey,
    /// An OAuth token of this kind.
    OAuth(OAuthCredentialKind),
    /// Whatever the row is (by-id authority of a gateway-verified credential).
    Any,
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
             WHERE s.credential_id = c.id) AS suspended,
    (SELECT jsonb_build_object(
                'kind', o.kind, 'grantId', g.id, 'appId', g.app_id, 'grantStatus', g.status,
                'scopes', to_jsonb(g.scopes), 'principalId', g.principal_id,
                'principalStatus', p.status, 'clientStatus', oc.status,
                'appActive', (oa.status = 'ACTIVE' AND ot.status = 'ACTIVE'))
       FROM platform_oauth_tokens o
       JOIN platform_oauth_grants g ON g.id = o.grant_id
       JOIN platform_principals p ON p.id = g.principal_id
       JOIN platform_oauth_clients oc ON oc.application_id = g.app_id
       JOIN platform_applications oa ON oa.id = g.app_id
       JOIN platform_tenants ot ON ot.id = oa.tenant_id
      WHERE o.credential_id = c.id) AS oauth
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
    oauth: Option<Value>,
}

/// The OAuth facts of a token row (ADR 0064).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthRow {
    kind: String,
    grant_id: Uuid,
    app_id: Uuid,
    grant_status: String,
    scopes: Vec<String>,
    principal_id: Uuid,
    principal_status: String,
    client_status: String,
    app_active: bool,
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

/// Section 15 (PLT-008): a credential's last use is recorded at most once per this many
/// seconds — per instance in memory, and across instances by the conditional upsert — so
/// authentication never writes on every request.
pub const USAGE_DEBOUNCE_SECONDS: i64 = 300;

/// Remembered last-recorded uses kept in memory before stale entries are dropped.
const USAGE_MEMORY_CAP: usize = 50_000;

/// In-memory debounce of last-use writes (`credential id -> last recorded`).
#[derive(Debug, Default)]
pub struct UsageDebounce {
    recorded: std::sync::Mutex<std::collections::HashMap<Uuid, DateTime<Utc>>>,
}

impl UsageDebounce {
    /// Whether a use of `credential` at `now` should be written (and, if so, remembers
    /// it). Bounded memory: past the cap, entries older than the window are dropped
    /// (and, if every entry is fresh, the map restarts — at worst one extra write each).
    pub fn should_record(&self, credential: Uuid, now: DateTime<Utc>) -> bool {
        let window = chrono::Duration::seconds(USAGE_DEBOUNCE_SECONDS);
        let mut map = self.recorded.lock().unwrap_or_else(|p| p.into_inner());
        if map.get(&credential).is_some_and(|at| now - *at < window) {
            return false;
        }
        if map.len() >= USAGE_MEMORY_CAP {
            map.retain(|_, at| now - *at < window);
            if map.len() >= USAGE_MEMORY_CAP {
                map.clear();
            }
        }
        map.insert(credential, now);
        true
    }
}

/// Verifies presented credentials against the persisted authority chain.
pub struct CredentialAuthenticator {
    db: DatabaseConnection,
    keyring: Arc<VerifierKeyring>,
    registry: Arc<ScopeRegistry>,
    clock: SharedClock,
    failures: AuthFailurePolicy,
    usage: UsageDebounce,
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
            usage: UsageDebounce::default(),
        }
    }

    /// Records that `credential` authenticated at `now` — debounced (see
    /// [`USAGE_DEBOUNCE_SECONDS`]) and best effort: a failure is logged, never turned
    /// into an authentication failure.
    async fn note_use(&self, credential: Uuid, now: DateTime<Utc>) {
        if !self.usage.should_record(credential, now) {
            return;
        }
        let written = self
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO platform_credential_usage (credential_id, last_authenticated_at) \
                 VALUES ($1, $2) \
                 ON CONFLICT (credential_id) DO UPDATE \
                     SET last_authenticated_at = EXCLUDED.last_authenticated_at \
                     WHERE platform_credential_usage.last_authenticated_at \
                           < EXCLUDED.last_authenticated_at - make_interval(secs => $3)",
                [
                    credential.into(),
                    now.into(),
                    (USAGE_DEBOUNCE_SECONDS as f64).into(),
                ],
            ))
            .await;
        if let Err(e) = written {
            tracing::warn!(summary = %summarize_db_error(&e), "credential last use not recorded");
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
        self.authenticate_family(presented, credential_format::API_CREDENTIAL, Expect::ApiKey)
            .await
    }

    /// ADR 0064: authenticates an OAuth access (`cfoa_`) or bot (`cfob_`) token. Refresh
    /// tokens (`cfor_`) and anything else are malformed here. The chain is the grant's
    /// target triple and installation service account; scopes are the grant's mapped
    /// internal scopes; the restriction is exactly the grant's principal.
    pub async fn authenticate_oauth(
        &self,
        presented: &str,
    ) -> Result<AuthorityChain, AuthenticationError> {
        let (family, kind) = if presented.starts_with("cfoa_") {
            (super::oauth::ACCESS_TOKEN, OAuthCredentialKind::Access)
        } else if presented.starts_with("cfob_") {
            (super::oauth::BOT_TOKEN, OAuthCredentialKind::Bot)
        } else {
            return Err(AuthenticationError::rejected(
                AuthFailureReason::MalformedCredential,
                None,
            ));
        };
        self.authenticate_family(presented, family, Expect::OAuth(kind))
            .await
    }

    async fn authenticate_family(
        &self,
        presented: &str,
        family: credential_format::CredentialFamily,
        expect: Expect,
    ) -> Result<AuthorityChain, AuthenticationError> {
        let Some(presented) = credential_format::parse_as(family, presented) else {
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
        let message = credential_format::verifier_message_as(
            family,
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

        let now = self.clock.now();
        let chain = evaluate(row, presented.tag_kind, now, &self.registry, expect)
            .map_err(|reason| AuthenticationError::rejected(reason, Some(&presented)))?;
        self.note_use(chain.credential.id, now).await;
        Ok(chain)
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
        evaluate(row, kind, self.clock.now(), &self.registry, Expect::Any)
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
    expect: Expect,
) -> Result<AuthorityChain, AuthFailureReason> {
    use AuthFailureReason as R;
    let corrupt = |_| R::CorruptAuthorityChain;
    let oauth: Option<OAuthRow> = row
        .oauth
        .clone()
        .map(serde_json::from_value)
        .transpose()
        .map_err(corrupt)?;
    let oauth_kind = match &oauth {
        Some(o) => Some(OAuthCredentialKind::from_token_kind(&o.kind).ok_or(R::CorruptAuthorityChain)?),
        None => None,
    };
    // A value of one family never authenticates as another (the verifier labels differ
    // too; this is the explicit second check).
    match (expect, oauth_kind) {
        (Expect::ApiKey, Some(_)) | (Expect::OAuth(_), None) => return Err(R::UnknownCredential),
        (Expect::OAuth(want), Some(have)) if want != have => return Err(R::UnknownCredential),
        _ => {}
    }
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
    // ADR 0064: an OAuth token lives only while its grant, app and principal do; its
    // scopes are the grant's (live), mapped through the closed catalogue, STANDARD only;
    // its restriction is exactly the grant's principal.
    let (scopes, credential_kind, app_id, grant_id) = match (oauth, oauth_kind) {
        (Some(o), Some(kind)) => {
            if o.grant_status != "ACTIVE" || o.client_status == "SUSPENDED" {
                return Err(R::CredentialRevoked);
            }
            if !o.app_active {
                return Err(R::CredentialSuspended);
            }
            match o.principal_status.as_str() {
                "ACTIVE" => {}
                "SUSPENDED" => return Err(R::CredentialSuspended),
                _ => return Err(R::CredentialRevoked),
            }
            if row.restricted_principals.as_deref() != Some(&[o.principal_id][..]) {
                return Err(R::CorruptAuthorityChain);
            }
            let scopes: Vec<String> =
                super::oauth_scopes::internal_scopes(o.scopes.iter().map(String::as_str))
                    .into_iter()
                    .filter(|s| {
                        registry
                            .definition(s)
                            .is_some_and(|d| d.trust == super::scopes::ScopeTrust::Standard)
                    })
                    .collect();
            (scopes, Some(kind), Some(o.app_id), Some(o.grant_id))
        }
        _ => (scopes, None, None, None),
    };
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
        credential_kind,
        app_id,
        grant_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_use_is_written_at_most_once_per_window_per_credential() {
        let d = UsageDebounce::default();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let t0 = DateTime::<Utc>::from_timestamp(1_790_000_000, 0).unwrap();
        let s = |n: i64| t0 + chrono::Duration::seconds(n);
        assert!(d.should_record(a, t0));
        assert!(!d.should_record(a, s(1)));
        assert!(!d.should_record(a, s(USAGE_DEBOUNCE_SECONDS - 1)));
        // Another credential has its own window.
        assert!(d.should_record(b, s(1)));
        // The window elapsed: one more write, then quiet again.
        assert!(d.should_record(a, s(USAGE_DEBOUNCE_SECONDS)));
        assert!(!d.should_record(a, s(USAGE_DEBOUNCE_SECONDS + 1)));
    }
}
