//! Credential authentication.
//!
//! One indexed query loads the presented credential's full authority chain (credential,
//! service account, environment, application, tenant and granted scopes). Authentication
//! succeeds only if:
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
//! context is returned in [`AuthorityMode::SafetyOnly`] and the authentication
//! middleware admits only the safety allow-list (Part H §72 / §74) — every other request
//! is refused exactly like an authentication failure.
//!
//! There is no cache: suspension, revocation and rotation take effect on the next
//! request. Clients only ever learn "authentication failed"; the specific
//! [`AuthFailureReason`] is for operators' logs (never with the secret).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sea_orm::{DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement};

use super::{
    context::{AuthorityMode, PlatformAuthContext, SuspendedLink, TrustedAuthority},
    credential_format::{self, PresentedCredential},
    credentials::CredentialEffectiveState,
    error::summarize_db_error,
    scopes::ScopeRegistry,
    verifier::{VerifierKeyring, VERIFIER_ALGORITHM},
};
use crate::{
    domain::{
        ApplicationId, ApplicationStatus, CredentialId, CredentialStatus, EnvironmentId,
        EnvironmentKind, EnvironmentStatus, RequestContext, ServiceAccountId, ServiceAccountStatus,
        SharedClock, TenantClassification, TenantId, TenantStatus,
    },
    models::api_credentials::Verifier,
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
    EnvironmentInactive,
    ApplicationInactive,
    TenantInactive,
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
            Self::EnvironmentInactive => "environment_inactive",
            Self::ApplicationInactive => "application_inactive",
            Self::TenantInactive => "tenant_inactive",
            Self::ScopeSuspended => "scope_suspended",
            Self::CorruptAuthorityChain => "corrupt_authority_chain",
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

const AUTHORITY_CHAIN_SQL: &str = r#"
SELECT
    c.id                       AS credential_id,
    c.verifier                 AS verifier,
    c.verifier_algorithm       AS verifier_algorithm,
    c.verifier_key_version     AS verifier_key_version,
    c.status                   AS credential_status,
    c.not_before               AS not_before,
    c.expires_at               AS expires_at,
    c.supersession_valid_until AS supersession_valid_until,
    sa.id                      AS service_account_id,
    sa.status                  AS service_account_status,
    e.id                       AS environment_id,
    e.kind                     AS environment_kind,
    e.status                   AS environment_status,
    e.live_telecom_allowed     AS live_telecom_allowed,
    a.id                       AS application_id,
    a.status                   AS application_status,
    t.id                       AS tenant_id,
    t.status                   AS tenant_status,
    t.classification           AS tenant_classification,
    ARRAY(
        SELECT cs.scope::text
        FROM platform_api_credential_scopes cs
        JOIN platform_application_scopes aps
          ON aps.application_id = cs.application_id AND aps.scope = cs.scope
        WHERE cs.credential_id = c.id
        ORDER BY cs.scope
    )                          AS scopes
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
    credential_id: uuid::Uuid,
    verifier: Verifier,
    verifier_algorithm: String,
    verifier_key_version: i32,
    credential_status: String,
    not_before: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    supersession_valid_until: Option<DateTime<Utc>>,
    service_account_id: uuid::Uuid,
    service_account_status: String,
    environment_id: uuid::Uuid,
    environment_kind: String,
    environment_status: String,
    live_telecom_allowed: bool,
    application_id: uuid::Uuid,
    application_status: String,
    tenant_id: uuid::Uuid,
    tenant_status: String,
    tenant_classification: String,
    scopes: Vec<String>,
}

/// Verifies presented credentials against the persisted authority chain.
pub struct CredentialAuthenticator {
    db: DatabaseConnection,
    keyring: Arc<VerifierKeyring>,
    registry: Arc<ScopeRegistry>,
    clock: SharedClock,
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
        }
    }

    /// Authenticates a presented credential string (the bearer token).
    pub async fn authenticate(
        &self,
        presented: &str,
        request: RequestContext,
    ) -> Result<PlatformAuthContext, AuthenticationError> {
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

        let authority = evaluate(&row, &presented, self.clock.now(), &self.registry)
            .map_err(|reason| AuthenticationError::rejected(reason, Some(&presented)))?;
        Ok(PlatformAuthContext::from_trusted(authority, request))
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
    row: &AuthorityRow,
    presented: &PresentedCredential,
    now: DateTime<Utc>,
    registry: &ScopeRegistry,
) -> Result<TrustedAuthority, AuthFailureReason> {
    use AuthFailureReason as R;
    let corrupt = || R::CorruptAuthorityChain;

    let environment_kind = EnvironmentKind::parse(&row.environment_kind).ok_or_else(corrupt)?;
    // The tag is bound into the verifier message too; this is an explicit second check.
    if environment_kind != presented.tag_kind {
        return Err(R::EnvironmentTagMismatch);
    }

    let status = CredentialStatus::parse(&row.credential_status).ok_or_else(corrupt)?;
    match CredentialEffectiveState::evaluate(
        status,
        row.not_before,
        row.expires_at,
        row.supersession_valid_until,
        now,
    ) {
        CredentialEffectiveState::Active | CredentialEffectiveState::RotationOverlap => {}
        CredentialEffectiveState::Revoked => return Err(R::CredentialRevoked),
        CredentialEffectiveState::NotYetValid => return Err(R::CredentialNotYetValid),
        CredentialEffectiveState::Expired => return Err(R::CredentialExpired),
        CredentialEffectiveState::Superseded => return Err(R::CredentialSuperseded),
    }

    if ServiceAccountStatus::parse(&row.service_account_status).ok_or_else(corrupt)?
        != ServiceAccountStatus::Active
    {
        return Err(R::ServiceAccountInactive);
    }
    // A SUSPENDED scope keeps a safety-only authority (Part H §72): the outermost
    // suspended link is reported. Unknown status values are a corrupt chain.
    let environment_status =
        EnvironmentStatus::parse(&row.environment_status).ok_or_else(corrupt)?;
    let application_status =
        ApplicationStatus::parse(&row.application_status).ok_or_else(corrupt)?;
    let tenant_status = TenantStatus::parse(&row.tenant_status).ok_or_else(corrupt)?;
    let authority_mode = if tenant_status != TenantStatus::Active {
        AuthorityMode::SafetyOnly(SuspendedLink::Tenant)
    } else if application_status != ApplicationStatus::Active {
        AuthorityMode::SafetyOnly(SuspendedLink::Application)
    } else if environment_status != EnvironmentStatus::Active {
        AuthorityMode::SafetyOnly(SuspendedLink::Environment)
    } else {
        AuthorityMode::Full
    };
    let classification =
        TenantClassification::parse(&row.tenant_classification).ok_or_else(corrupt)?;

    Ok(TrustedAuthority {
        authority_mode,
        tenant_id: TenantId::from_uuid(row.tenant_id),
        tenant_classification: classification,
        application_id: ApplicationId::from_uuid(row.application_id),
        environment_id: EnvironmentId::from_uuid(row.environment_id),
        environment_kind,
        live_telecom_allowed: row.live_telecom_allowed,
        service_account_id: ServiceAccountId::from_uuid(row.service_account_id),
        credential_id: CredentialId::from_uuid(row.credential_id),
        scopes: registry.effective(row.scopes.iter().cloned(), classification),
    })
}
