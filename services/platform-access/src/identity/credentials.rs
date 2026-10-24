//! API credential lifecycle: issuance, rotation with overlap, revocation, safe views.
//!
//! * The raw credential is returned exactly once, inside [`IssuedCredential`], by
//!   issuance and rotation. Only the HMAC verifier is persisted; every read path returns
//!   [`CredentialView`], which has no secret or verifier field.
//! * A credential is bound to one service account and therefore to one environment,
//!   application and tenant (composite foreign key, immutable columns).
//! * Granted scopes must be registry scopes allowed for the application.
//! * Rotation creates an independent successor and limits the predecessor to
//!   `min(original expiry, now + overlap)`; it never extends the predecessor.
//! * Revocation is immediate and idempotent; authentication reads the database on every
//!   request, so there is no cache to invalidate.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait,
    DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use serde::Serialize;

use super::{
    credential_format, optional_text, scopes::allowed_scopes, verifier::VERIFIER_ALGORITHM,
    PlatformError, PlatformResult, PlatformService,
};
use crate::{
    domain::{
        ApplicationId, ApplicationStatus, CredentialId, CredentialStatus, EnvironmentId,
        EnvironmentKind, EnvironmentStatus, IssuedSecret, ServiceAccountId, ServiceAccountStatus,
        TenantClassification, TenantId, TenantStatus,
    },
    models::{
        api_credential_scopes, api_credentials, applications, environments, service_accounts,
        tenants,
    },
};

/// Time-dependent state of a credential, derived from persisted fields. Authentication
/// accepts only [`Active`](CredentialEffectiveState::Active) and
/// [`RotationOverlap`](CredentialEffectiveState::RotationOverlap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CredentialEffectiveState {
    Active,
    /// Superseded by rotation, still inside its overlap window.
    RotationOverlap,
    NotYetValid,
    Expired,
    /// Superseded by rotation; overlap window ended.
    Superseded,
    Revoked,
    /// Reversibly suspended (Part VI §54); never derived from the persisted status.
    Suspended,
}

impl CredentialEffectiveState {
    pub fn evaluate(
        status: CredentialStatus,
        not_before: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        supersession_valid_until: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        if status == CredentialStatus::Revoked {
            return Self::Revoked;
        }
        if now < not_before {
            return Self::NotYetValid;
        }
        if now >= expires_at {
            return Self::Expired;
        }
        match supersession_valid_until {
            Some(until) if now >= until => Self::Superseded,
            Some(_) => Self::RotationOverlap,
            None => Self::Active,
        }
    }

    pub fn authenticates(&self) -> bool {
        matches!(self, Self::Active | Self::RotationOverlap)
    }

    /// The state with a suspension applied: a suspension masks an authenticating state
    /// (revoked / expired / superseded stay what they are).
    pub fn with_suspension(self, suspended: bool) -> Self {
        if suspended && self.authenticates() {
            Self::Suspended
        } else {
            self
        }
    }
}

/// Safe representation of a credential: no secret, no verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialView {
    pub id: CredentialId,
    pub public_id: String,
    pub masked: String,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub environment_kind: EnvironmentKind,
    pub service_account_id: ServiceAccountId,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub status: CredentialStatus,
    pub effective_state: CredentialEffectiveState,
    pub not_before: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub rotated_from_credential_id: Option<CredentialId>,
    pub superseded_by_credential_id: Option<CredentialId>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub supersession_valid_until: Option<DateTime<Utc>>,
    /// When the credential was suspended (Part VI §54; `None`: not suspended).
    pub suspended_at: Option<DateTime<Utc>>,
    pub verifier_algorithm: String,
    pub verifier_key_version: i32,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Result of issuance / rotation. The only value that ever carries the raw credential.
/// Deliberately not `Serialize` / `Clone`; `Debug` redacts the secret.
#[derive(Debug)]
pub struct IssuedCredential {
    pub credential: CredentialView,
    pub secret: IssuedSecret,
}

#[derive(Debug, Clone, Default)]
pub struct IssueCredential {
    /// Requested scopes; each must be a registry scope allowed for the application.
    pub scopes: Vec<String>,
    pub label: Option<String>,
    /// Defaults to now; may not be in the past.
    pub not_before: Option<DateTime<Utc>>,
    /// Defaults to `not_before + default lifetime`; capped by the maximum lifetime.
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
pub struct RotateCredential {
    /// How long the predecessor keeps working; defaults to the configured overlap.
    pub overlap: Option<Duration>,
    /// Replacement scope set; defaults to the predecessor's scopes (re-validated).
    pub scopes: Option<Vec<String>>,
    /// Defaults to the predecessor's label.
    pub label: Option<String>,
    /// Successor expiry; defaults to now + default lifetime.
    pub expires_at: Option<DateTime<Utc>>,
}

/// Which credentials to list.
#[derive(Debug, Clone, Copy)]
pub enum CredentialFilter {
    ServiceAccount(ServiceAccountId),
    Environment(EnvironmentId),
    Application(ApplicationId),
}

/// The issuing chain of a service account, loaded inside the issuing transaction.
struct IssuingChain {
    service_account: service_accounts::Model,
    environment_kind: EnvironmentKind,
    classification: TenantClassification,
}

impl PlatformService {
    async fn issuing_chain(
        &self,
        txn: &DatabaseTransaction,
        service_account_id: ServiceAccountId,
    ) -> PlatformResult<IssuingChain> {
        let service_account = service_accounts::Entity::find_by_id(service_account_id.as_uuid())
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("service account"))?;
        let environment = environments::Entity::find_by_id(service_account.environment_id)
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("environment"))?;
        let application = applications::Entity::find_by_id(service_account.application_id)
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("application"))?;
        let tenant = tenants::Entity::find_by_id(service_account.tenant_id)
            .one(txn)
            .await?
            .ok_or(PlatformError::NotFound("tenant"))?;
        let inactive = if service_account.status != ServiceAccountStatus::Active {
            Some("service account")
        } else if environment.status != EnvironmentStatus::Active {
            Some("environment")
        } else if application.status != ApplicationStatus::Active {
            Some("application")
        } else if tenant.status != TenantStatus::Active {
            Some("tenant")
        } else {
            None
        };
        if let Some(what) = inactive {
            return Err(PlatformError::InvalidState(format!(
                "credentials cannot be issued while the {what} is not ACTIVE"
            )));
        }
        Ok(IssuingChain {
            service_account,
            environment_kind: environment.kind,
            classification: tenant.classification,
        })
    }

    /// Registry + trust-classification + application-allowed-set + environment-kind
    /// restriction validation.
    async fn validated_scopes(
        &self,
        txn: &DatabaseTransaction,
        application_id: ApplicationId,
        classification: TenantClassification,
        environment_kind: EnvironmentKind,
        requested: &[String],
    ) -> PlatformResult<BTreeSet<String>> {
        let requested = self
            .registry
            .validate_requested(requested, classification)?;
        let allowed = allowed_scopes(txn, application_id).await?;
        let mut granted = BTreeSet::new();
        for scope in requested {
            let Some(grant) = allowed.get(scope.as_str()) else {
                return Err(PlatformError::PolicyViolation(format!(
                    "scope `{}` is not allowed for this application",
                    scope.as_str()
                )));
            };
            if self.registry.is_privileged(scope.as_str())
                && grant.grant_basis != super::scopes::GrantBasis::PrivilegedOperator
            {
                return Err(PlatformError::PolicyViolation(format!(
                    "scope `{}` requires a privileged operator grant on the application",
                    scope.as_str()
                )));
            }
            if !grant.allows(environment_kind) {
                return Err(PlatformError::PolicyViolation(format!(
                    "scope `{}` is not allowed in {environment_kind} environments of this application",
                    scope.as_str()
                )));
            }
            granted.insert(scope.as_str().to_string());
        }
        Ok(granted)
    }

    fn validity_window(
        &self,
        now: DateTime<Utc>,
        not_before: Option<DateTime<Utc>>,
        expires_at: Option<DateTime<Utc>>,
    ) -> PlatformResult<(DateTime<Utc>, DateTime<Utc>)> {
        let not_before = not_before.unwrap_or(now);
        if not_before < now {
            return Err(PlatformError::Validation(
                "not_before may not be in the past".to_string(),
            ));
        }
        let expires_at = expires_at.unwrap_or(not_before + self.policy.default_lifetime);
        if expires_at <= not_before {
            return Err(PlatformError::Validation(
                "expires_at must be after not_before".to_string(),
            ));
        }
        if expires_at > now + self.policy.max_lifetime {
            return Err(PlatformError::PolicyViolation(format!(
                "credential lifetime may not exceed {} days",
                self.policy.max_lifetime.num_days()
            )));
        }
        Ok((not_before, expires_at))
    }

    /// Inserts a credential + its scopes inside `txn` and returns the raw secret.
    #[allow(clippy::too_many_arguments)]
    async fn insert_credential(
        &self,
        txn: &DatabaseTransaction,
        chain: &IssuingChain,
        scopes: &BTreeSet<String>,
        label: Option<String>,
        not_before: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        rotated_from: Option<CredentialId>,
        now: DateTime<Utc>,
    ) -> PlatformResult<(api_credentials::Model, IssuedSecret)> {
        let generated = credential_format::generate(chain.environment_kind);
        let message = credential_format::verifier_message(
            chain.environment_kind,
            &generated.public_id,
            &generated.secret,
        );
        let verifier = self.keyring.compute_current(&message);
        let sa = &chain.service_account;
        let model = api_credentials::ActiveModel {
            id: Set(CredentialId::generate().as_uuid()),
            public_id: Set(generated.public_id.clone()),
            tenant_id: Set(sa.tenant_id),
            application_id: Set(sa.application_id),
            environment_id: Set(sa.environment_id),
            service_account_id: Set(sa.id),
            label: Set(label),
            verifier: Set(api_credentials::Verifier(verifier.to_vec())),
            verifier_algorithm: Set(VERIFIER_ALGORITHM.to_string()),
            verifier_key_version: Set(self.keyring.current_version()),
            last_four: Set(generated.last_four.clone()),
            status: Set(CredentialStatus::Active),
            not_before: Set(not_before),
            expires_at: Set(expires_at),
            revoked_at: Set(None),
            rotated_from_credential_id: Set(rotated_from.map(|id| id.as_uuid())),
            superseded_by_credential_id: Set(None),
            superseded_at: Set(None),
            supersession_valid_until: Set(None),
            revision: sea_orm::ActiveValue::NotSet,
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
        if !scopes.is_empty() {
            api_credential_scopes::Entity::insert_many(scopes.iter().map(|scope| {
                api_credential_scopes::ActiveModel {
                    credential_id: Set(model.id),
                    scope: Set(scope.clone()),
                    tenant_id: Set(model.tenant_id),
                    application_id: Set(model.application_id),
                    granted_at: Set(now),
                }
            }))
            .exec_without_returning(txn)
            .await?;
        }
        Ok((model, generated.token))
    }

    /// Issues a new credential for an ACTIVE service account. The raw credential is in
    /// the result and nowhere else.
    pub async fn issue_credential(
        &self,
        service_account_id: ServiceAccountId,
        request: IssueCredential,
    ) -> PlatformResult<IssuedCredential> {
        self.issue_credential_in(&self.db, service_account_id, request)
            .await
    }

    pub(crate) async fn issue_credential_in<C: ConnectionTrait + TransactionTrait>(
        &self,
        db: &C,
        service_account_id: ServiceAccountId,
        request: IssueCredential,
    ) -> PlatformResult<IssuedCredential> {
        let label = optional_text(request.label.as_deref(), "label", 100)?;
        let now = self.clock.now();
        let (not_before, expires_at) =
            self.validity_window(now, request.not_before, request.expires_at)?;

        let txn = db.begin().await?;
        let chain = self.issuing_chain(&txn, service_account_id).await?;
        let scopes = self
            .validated_scopes(
                &txn,
                ApplicationId::from_uuid(chain.service_account.application_id),
                chain.classification,
                chain.environment_kind,
                &request.scopes,
            )
            .await?;
        let (model, secret) = self
            .insert_credential(
                &txn, &chain, &scopes, label, not_before, expires_at, None, now,
            )
            .await?;
        txn.commit().await?;

        tracing::info!(
            credential_id = %model.id,
            public_id = %model.public_id,
            service_account_id = %model.service_account_id,
            environment_id = %model.environment_id,
            "API credential issued"
        );
        let credential = self
            .credential_in(db, CredentialId::from_uuid(model.id))
            .await?;
        Ok(IssuedCredential { credential, secret })
    }

    /// Rotates an ACTIVE credential: issues an independent successor bound to the same
    /// service account and limits the predecessor to `min(expiry, now + overlap)`.
    pub async fn rotate_credential(
        &self,
        credential_id: CredentialId,
        request: RotateCredential,
    ) -> PlatformResult<IssuedCredential> {
        self.rotate_credential_in(&self.db, credential_id, request)
            .await
    }

    pub(crate) async fn rotate_credential_in<C: ConnectionTrait + TransactionTrait>(
        &self,
        db: &C,
        credential_id: CredentialId,
        request: RotateCredential,
    ) -> PlatformResult<IssuedCredential> {
        let overlap = request
            .overlap
            .unwrap_or(self.policy.default_rotation_overlap);
        if overlap < Duration::zero() || overlap > self.policy.max_rotation_overlap {
            return Err(PlatformError::PolicyViolation(format!(
                "rotation overlap must be between 0 and {} minutes",
                self.policy.max_rotation_overlap.num_minutes()
            )));
        }
        let now = self.clock.now();
        let (not_before, expires_at) = self.validity_window(now, None, request.expires_at)?;

        let txn = db.begin().await?;
        let old = api_credentials::Entity::find_by_id(credential_id.as_uuid())
            .lock_exclusive()
            .one(&txn)
            .await?
            .ok_or(PlatformError::NotFound("credential"))?;
        if old.superseded_by_credential_id.is_some() {
            return Err(PlatformError::InvalidState(
                "credential has already been rotated; rotate its successor instead".to_string(),
            ));
        }
        let state = CredentialEffectiveState::evaluate(
            old.status,
            old.not_before,
            old.expires_at,
            old.supersession_valid_until,
            now,
        );
        let state = state.with_suspension(!suspensions_of(&txn, &[old.id]).await?.is_empty());
        if state != CredentialEffectiveState::Active {
            return Err(PlatformError::InvalidState(format!(
                "only an ACTIVE credential can be rotated (current state: {state:?})"
            )));
        }
        let chain = self
            .issuing_chain(&txn, ServiceAccountId::from_uuid(old.service_account_id))
            .await?;
        let requested_scopes = match request.scopes {
            Some(scopes) => scopes,
            None => api_credential_scopes::Entity::find()
                .filter(api_credential_scopes::Column::CredentialId.eq(old.id))
                .all(&txn)
                .await?
                .into_iter()
                .map(|row| row.scope)
                .collect(),
        };
        let scopes = self
            .validated_scopes(
                &txn,
                ApplicationId::from_uuid(old.application_id),
                chain.classification,
                chain.environment_kind,
                &requested_scopes,
            )
            .await?;
        let label = match request.label {
            Some(label) => optional_text(Some(&label), "label", 100)?,
            None => old.label.clone(),
        };
        let (new, secret) = self
            .insert_credential(
                &txn,
                &chain,
                &scopes,
                label,
                not_before,
                expires_at,
                Some(credential_id),
                now,
            )
            .await?;

        // Credential-level limit policy belongs to the consuming services (Calling's
        // limits): they carry it over when they observe the successor's
        // `rotated_from_credential_id` in the identity change feed.

        let valid_until = std::cmp::min(old.expires_at, now + overlap);
        let mut predecessor: api_credentials::ActiveModel = old.into();
        predecessor.superseded_by_credential_id = Set(Some(new.id));
        predecessor.superseded_at = Set(Some(now));
        predecessor.supersession_valid_until = Set(Some(valid_until));
        predecessor.updated_at = Set(now);
        predecessor.update(&txn).await?;
        txn.commit().await?;

        tracing::info!(
            credential_id = %new.id,
            public_id = %new.public_id,
            rotated_from = %credential_id,
            predecessor_valid_until = %valid_until,
            "API credential rotated"
        );
        let credential = self
            .credential_in(db, CredentialId::from_uuid(new.id))
            .await?;
        Ok(IssuedCredential { credential, secret })
    }

    /// Revokes a credential immediately. Idempotent: revoking a revoked credential
    /// returns it unchanged.
    pub async fn revoke_credential(
        &self,
        credential_id: CredentialId,
    ) -> PlatformResult<CredentialView> {
        self.revoke_credential_in(&self.db, credential_id).await
    }

    pub(crate) async fn revoke_credential_in<C: ConnectionTrait>(
        &self,
        db: &C,
        credential_id: CredentialId,
    ) -> PlatformResult<CredentialView> {
        let now = self.clock.now();
        let result = api_credentials::Entity::update_many()
            .col_expr(
                api_credentials::Column::Status,
                Expr::value(CredentialStatus::Revoked.as_str()),
            )
            .col_expr(api_credentials::Column::RevokedAt, Expr::value(now))
            .col_expr(api_credentials::Column::UpdatedAt, Expr::value(now))
            .filter(api_credentials::Column::Id.eq(credential_id.as_uuid()))
            .filter(api_credentials::Column::Status.eq(CredentialStatus::Active))
            .exec(db)
            .await?;
        let view = self.credential_in(db, credential_id).await?;
        if result.rows_affected > 0 {
            tracing::info!(credential_id = %credential_id, public_id = %view.public_id, "API credential revoked");
        }
        Ok(view)
    }

    /// Suspends a credential (reversible; Part VI §54): it stops authenticating at once.
    /// Only a credential that still authenticates can be suspended; suspending a
    /// suspended credential returns it unchanged. The credential row is touched so the
    /// identity change feed signals the change.
    pub(crate) async fn suspend_credential_in<C: ConnectionTrait>(
        &self,
        db: &C,
        credential_id: CredentialId,
        by: &str,
        reason_code: &str,
    ) -> PlatformResult<(CredentialView, bool)> {
        let view = self.credential_in(db, credential_id).await?;
        if view.effective_state == CredentialEffectiveState::Suspended {
            return Ok((view, false));
        }
        if !view.effective_state.authenticates() {
            return Err(PlatformError::InvalidState(format!(
                "only an authenticating credential can be suspended (current state: {:?})",
                view.effective_state
            )));
        }
        let valid_reason = (2..=64).contains(&reason_code.len())
            && reason_code.as_bytes()[0].is_ascii_uppercase()
            && reason_code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !valid_reason {
            return Err(PlatformError::Validation(
                "reason_code must be an UPPER_SNAKE code".to_string(),
            ));
        }
        let now = self.clock.now();
        db.execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "INSERT INTO platform_credential_suspensions \
               (credential_id, tenant_id, suspended_at, suspended_by, reason_code) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (credential_id) DO NOTHING",
            [
                credential_id.as_uuid().into(),
                view.tenant_id.as_uuid().into(),
                now.into(),
                by.chars().take(100).collect::<String>().into(),
                reason_code.into(),
            ],
        ))
        .await?;
        self.touch_credential(db, credential_id, now).await?;
        tracing::info!(credential_id = %credential_id, "API credential suspended");
        Ok((self.credential_in(db, credential_id).await?, true))
    }

    /// Lifts a suspension (idempotent: an unsuspended credential is returned unchanged).
    pub(crate) async fn resume_credential_in<C: ConnectionTrait>(
        &self,
        db: &C,
        credential_id: CredentialId,
    ) -> PlatformResult<(CredentialView, bool)> {
        self.credential_in(db, credential_id).await?;
        let removed = db
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "DELETE FROM platform_credential_suspensions WHERE credential_id = $1",
                [credential_id.as_uuid().into()],
            ))
            .await?
            .rows_affected()
            > 0;
        if removed {
            self.touch_credential(db, credential_id, self.clock.now())
                .await?;
            tracing::info!(credential_id = %credential_id, "API credential resumed");
        }
        Ok((self.credential_in(db, credential_id).await?, removed))
    }

    async fn touch_credential<C: ConnectionTrait>(
        &self,
        db: &C,
        credential_id: CredentialId,
        now: DateTime<Utc>,
    ) -> PlatformResult<()> {
        api_credentials::Entity::update_many()
            .col_expr(api_credentials::Column::UpdatedAt, Expr::value(now))
            .filter(api_credentials::Column::Id.eq(credential_id.as_uuid()))
            .exec(db)
            .await?;
        Ok(())
    }

    /// Whether `id` is currently suspended.
    pub async fn credential_suspended(&self, id: CredentialId) -> PlatformResult<bool> {
        Ok(!suspensions_of(&self.db, &[id.as_uuid()]).await?.is_empty())
    }

    pub async fn credential(&self, id: CredentialId) -> PlatformResult<CredentialView> {
        self.credential_in(&self.db, id).await
    }

    pub(crate) async fn credential_in<C: ConnectionTrait>(
        &self,
        db: &C,
        id: CredentialId,
    ) -> PlatformResult<CredentialView> {
        let model = api_credentials::Entity::find_by_id(id.as_uuid())
            .one(db)
            .await?
            .ok_or(PlatformError::NotFound("credential"))?;
        Ok(self.views_in(db, vec![model]).await?.remove(0))
    }

    pub async fn credential_by_public_id(&self, public_id: &str) -> PlatformResult<CredentialView> {
        let model = api_credentials::Entity::find()
            .filter(api_credentials::Column::PublicId.eq(public_id))
            .one(&self.db)
            .await?
            .ok_or(PlatformError::NotFound("credential"))?;
        Ok(self.views(vec![model]).await?.remove(0))
    }

    pub async fn list_credentials(
        &self,
        filter: CredentialFilter,
    ) -> PlatformResult<Vec<CredentialView>> {
        let query = api_credentials::Entity::find();
        let query = match filter {
            CredentialFilter::ServiceAccount(id) => {
                query.filter(api_credentials::Column::ServiceAccountId.eq(id.as_uuid()))
            }
            CredentialFilter::Environment(id) => {
                query.filter(api_credentials::Column::EnvironmentId.eq(id.as_uuid()))
            }
            CredentialFilter::Application(id) => {
                query.filter(api_credentials::Column::ApplicationId.eq(id.as_uuid()))
            }
        };
        let models = query
            .order_by_asc(api_credentials::Column::CreatedAt)
            .order_by_asc(api_credentials::Column::Id)
            .all(&self.db)
            .await?;
        self.views(models).await
    }

    async fn views(
        &self,
        models: Vec<api_credentials::Model>,
    ) -> PlatformResult<Vec<CredentialView>> {
        self.views_in(&self.db, models).await
    }

    async fn views_in<C: ConnectionTrait>(
        &self,
        db: &C,
        models: Vec<api_credentials::Model>,
    ) -> PlatformResult<Vec<CredentialView>> {
        if models.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.clock.now();
        let ids: Vec<_> = models.iter().map(|m| m.id).collect();
        let env_ids: BTreeSet<_> = models.iter().map(|m| m.environment_id).collect();
        let kinds: HashMap<_, _> = environments::Entity::find()
            .filter(environments::Column::Id.is_in(env_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|e| (e.id, e.kind))
            .collect();
        let suspensions = suspensions_of(db, &ids).await?;
        let mut scopes: HashMap<_, Vec<String>> = HashMap::new();
        for row in scopes_of(db, ids).await? {
            scopes.entry(row.credential_id).or_default().push(row.scope);
        }
        models
            .into_iter()
            .map(|m| {
                let kind = *kinds
                    .get(&m.environment_id)
                    .ok_or(PlatformError::NotFound("environment"))?;
                Ok(CredentialView {
                    id: CredentialId::from_uuid(m.id),
                    masked: credential_format::masked(kind, &m.public_id, &m.last_four),
                    public_id: m.public_id,
                    tenant_id: TenantId::from_uuid(m.tenant_id),
                    application_id: ApplicationId::from_uuid(m.application_id),
                    environment_id: EnvironmentId::from_uuid(m.environment_id),
                    environment_kind: kind,
                    service_account_id: ServiceAccountId::from_uuid(m.service_account_id),
                    label: m.label,
                    scopes: scopes.remove(&m.id).unwrap_or_default(),
                    status: m.status,
                    effective_state: CredentialEffectiveState::evaluate(
                        m.status,
                        m.not_before,
                        m.expires_at,
                        m.supersession_valid_until,
                        now,
                    )
                    .with_suspension(suspensions.contains_key(&m.id)),
                    suspended_at: suspensions.get(&m.id).copied(),
                    not_before: m.not_before,
                    expires_at: m.expires_at,
                    revoked_at: m.revoked_at,
                    rotated_from_credential_id: m
                        .rotated_from_credential_id
                        .map(CredentialId::from_uuid),
                    superseded_by_credential_id: m
                        .superseded_by_credential_id
                        .map(CredentialId::from_uuid),
                    superseded_at: m.superseded_at,
                    supersession_valid_until: m.supersession_valid_until,
                    verifier_algorithm: m.verifier_algorithm,
                    verifier_key_version: m.verifier_key_version,
                    revision: m.revision,
                    created_at: m.created_at,
                    updated_at: m.updated_at,
                })
            })
            .collect()
    }
}

/// `credential id -> suspended_at` of the suspended credentials among `ids`.
pub(crate) async fn suspensions_of<C: ConnectionTrait>(
    db: &C,
    ids: &[uuid::Uuid],
) -> PlatformResult<HashMap<uuid::Uuid, DateTime<Utc>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db
        .query_all(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT credential_id, suspended_at FROM platform_credential_suspensions \
             WHERE credential_id = ANY($1)",
            [ids.to_vec().into()],
        ))
        .await?;
    let mut out = HashMap::new();
    for r in rows {
        out.insert(
            r.try_get::<uuid::Uuid>("", "credential_id")?,
            r.try_get::<DateTime<Utc>>("", "suspended_at")?,
        );
    }
    Ok(out)
}

async fn scopes_of<C: ConnectionTrait>(
    db: &C,
    credential_ids: Vec<uuid::Uuid>,
) -> PlatformResult<Vec<api_credential_scopes::Model>> {
    Ok(api_credential_scopes::Entity::find()
        .filter(api_credential_scopes::Column::CredentialId.is_in(credential_ids))
        .order_by_asc(api_credential_scopes::Column::Scope)
        .all(db)
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(minutes)
    }

    #[test]
    fn effective_state_is_derived_from_persisted_fields() {
        use CredentialEffectiveState as S;
        let active = CredentialStatus::Active;
        assert_eq!(
            S::evaluate(active, t(0), t(100), None, t(-1)),
            S::NotYetValid
        );
        assert_eq!(S::evaluate(active, t(0), t(100), None, t(0)), S::Active);
        assert_eq!(S::evaluate(active, t(0), t(100), None, t(99)), S::Active);
        assert_eq!(S::evaluate(active, t(0), t(100), None, t(100)), S::Expired);
        assert_eq!(
            S::evaluate(active, t(0), t(100), Some(t(50)), t(49)),
            S::RotationOverlap
        );
        assert_eq!(
            S::evaluate(active, t(0), t(100), Some(t(50)), t(50)),
            S::Superseded
        );
        assert_eq!(
            S::evaluate(CredentialStatus::Revoked, t(0), t(100), None, t(10)),
            S::Revoked
        );
        assert_eq!(
            S::evaluate(CredentialStatus::Revoked, t(0), t(100), Some(t(50)), t(10)),
            S::Revoked
        );
    }

    #[test]
    fn only_active_and_overlap_states_authenticate() {
        use CredentialEffectiveState as S;
        assert!(S::Active.authenticates());
        assert!(S::RotationOverlap.authenticates());
        for s in [S::NotYetValid, S::Expired, S::Superseded, S::Revoked] {
            assert!(!s.authenticates());
        }
    }
}
