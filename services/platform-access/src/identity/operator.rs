//! Operator audit of identity administration (Part H §72.8 carried over from Calling;
//! table `platform_access_audit`).
//!
//! Every mutating administration operation takes an explicit [`OperatorContext`] — WHO
//! acts and the correlation id of the command — and writes exactly ONE audit row in the
//! SAME transaction as the mutation: the row exists if and only if the change committed.
//! The audited operations are the `operator_*` methods below (and the operator functions
//! of the modules that own the other administered resources, which call
//! [`record`]); `platform_admin` reaches mutations only through them, so the audit write
//! cannot be skipped by the CLI.
//!
//! `detail` holds opaque ids and bounded codes only — never a credential secret, a
//! verifier, the pepper, a contact value or a number (the table refuses the obvious keys
//! and anything over 2 KiB). The table is append-only (UPDATE / DELETE / TRUNCATE are
//! refused by trigger).

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    applications::{ApplicationView, NewApplication},
    credentials::{CredentialView, IssueCredential, IssuedCredential, RotateCredential},
    environments::EnvironmentView,
    principals::{NewPrincipal, PrincipalView},
    scopes::ApplicationScopeView,
    service_accounts::{NewServiceAccount, ServiceAccountView},
    tenants::{NewTenant, TenantView},
    PlatformError, PlatformResult, PlatformService,
};
use crate::domain::{
    ApplicationId, ApplicationStatus, CredentialId, EnvironmentId, EnvironmentKind,
    EnvironmentStatus, PrincipalId, PrincipalStatus, ServiceAccountId, ServiceAccountStatus,
    TenantId, TenantStatus,
};

/// The environment variable `platform-access-admin` reads the operator id from when
/// `--operator` is absent.
pub const OPERATOR_ENV: &str = "CF_OPERATOR_ID";

text_enum!(
    /// Closed vocabulary of audited operator actions (the table's CHECK).
    OperatorAction {
        TenantCreated = "TENANT_CREATED",
        TenantStatusSet = "TENANT_STATUS_SET",
        ApplicationCreated = "APPLICATION_CREATED",
        ApplicationStatusSet = "APPLICATION_STATUS_SET",
        EnvironmentCreated = "ENVIRONMENT_CREATED",
        EnvironmentStatusSet = "ENVIRONMENT_STATUS_SET",
        EnvironmentLiveTelecomSet = "ENVIRONMENT_LIVE_TELECOM_SET",
        ServiceAccountCreated = "SERVICE_ACCOUNT_CREATED",
        ServiceAccountStatusSet = "SERVICE_ACCOUNT_STATUS_SET",
        CredentialCreated = "CREDENTIAL_CREATED",
        CredentialRotated = "CREDENTIAL_ROTATED",
        CredentialRevoked = "CREDENTIAL_REVOKED",
        CredentialPrincipalsRestricted = "CREDENTIAL_PRINCIPALS_RESTRICTED",
        ScopeGranted = "SCOPE_GRANTED",
        ScopeRevoked = "SCOPE_REVOKED",
        PrivilegedScopeGranted = "PRIVILEGED_SCOPE_GRANTED",
        PrincipalRegistered = "PRINCIPAL_REGISTERED",
        PrincipalStatusSet = "PRINCIPAL_STATUS_SET",
        PrincipalTombstoned = "PRINCIPAL_TOMBSTONED",
        IdentityImported = "IDENTITY_IMPORTED",
        CredentialSuspended = "CREDENTIAL_SUSPENDED",
        CredentialResumed = "CREDENTIAL_RESUMED",
        ProductionEnvironmentActivated = "PRODUCTION_ENVIRONMENT_ACTIVATED",
    }
);

text_enum!(
    /// What an audited action is about (an opaque id of that kind).
    AuditTargetKind {
        Tenant = "TENANT",
        Application = "APPLICATION",
        Environment = "ENVIRONMENT",
        ServiceAccount = "SERVICE_ACCOUNT",
        Credential = "CREDENTIAL",
        Principal = "PRINCIPAL",
        IdentityImport = "IDENTITY_IMPORT",
    }
);

/// Who performs an administration command, and the correlation id of that command.
/// Constructed only from a validated operator id: there is no default operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorContext {
    operator: String,
    correlation_id: String,
}

fn valid_operator(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 100
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._@-".contains(b))
}

impl OperatorContext {
    /// `operator` must match `^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$` (an opaque operator
    /// id such as an SSO handle); a fresh correlation id is generated.
    pub fn new(operator: &str) -> PlatformResult<Self> {
        let operator = operator.trim();
        if !valid_operator(operator) {
            return Err(PlatformError::Validation(
                "the operator id must match ^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$".to_string(),
            ));
        }
        Ok(Self {
            operator: operator.to_string(),
            correlation_id: format!("op_{}", Uuid::new_v4().simple()),
        })
    }

    pub fn operator(&self) -> &str {
        &self.operator
    }

    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }
}

/// One audited action: what, on which target (scoped), with bounded detail.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub action: OperatorAction,
    pub target_kind: AuditTargetKind,
    pub target_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub detail: Value,
}

impl AuditEntry {
    pub fn new(action: OperatorAction, target_kind: AuditTargetKind, target_id: Uuid) -> Self {
        Self {
            action,
            target_kind,
            target_id: Some(target_id),
            tenant_id: None,
            application_id: None,
            environment_id: None,
            detail: json!({}),
        }
    }

    /// The owning scope (any level may be absent, e.g. a tenant-level action).
    pub fn scope(
        mut self,
        tenant_id: Option<Uuid>,
        application_id: Option<Uuid>,
        environment_id: Option<Uuid>,
    ) -> Self {
        self.tenant_id = tenant_id;
        self.application_id = application_id;
        self.environment_id = environment_id;
        self
    }

    /// Bounded detail: opaque ids and codes only.
    pub fn detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }
}

/// Writes the audit row of `entry` with `db` — which must be the transaction of the
/// mutation it describes. A failure fails the whole operation (no unaudited change).
pub async fn record<C: ConnectionTrait>(
    db: &C,
    operator: &OperatorContext,
    entry: &AuditEntry,
    at: DateTime<Utc>,
) -> PlatformResult<Uuid> {
    let id = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO platform_access_audit (id, actor, correlation_id, action, target_kind, \
           target_id, tenant_id, application_id, environment_id, detail, occurred_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        [
            id.into(),
            operator.operator().into(),
            operator.correlation_id().into(),
            entry.action.as_str().into(),
            entry.target_kind.as_str().into(),
            entry.target_id.into(),
            entry.tenant_id.into(),
            entry.application_id.into(),
            entry.environment_id.into(),
            entry.detail.clone().into(),
            at.into(),
        ],
    ))
    .await?;
    crate::metrics::inc(
        "platform_access_operator_actions_total",
        &[("action", entry.action.as_str())],
    );
    tracing::info!(
        operator = operator.operator(),
        correlation_id = operator.correlation_id(),
        action = entry.action.as_str(),
        target_kind = entry.target_kind.as_str(),
        target_id = ?entry.target_id,
        "operator action audited"
    );
    Ok(id)
}

/// Runs `mutation` in a fresh transaction of `db`, writes its audit entry in that same
/// transaction and commits — all or nothing.
pub async fn audited<T, F>(
    db: &DatabaseConnection,
    operator: &OperatorContext,
    at: DateTime<Utc>,
    mutation: F,
) -> PlatformResult<T>
where
    F: for<'t> FnOnce(&'t DatabaseTransaction) -> BoxFuture<'t, PlatformResult<(T, AuditEntry)>>,
{
    let txn = db.begin().await?;
    let (value, entry) = mutation(&txn).await?;
    record(&txn, operator, &entry, at).await?;
    txn.commit().await?;
    Ok(value)
}

/// Opens the mutation's transaction, runs it, audits it and commits — all or nothing.
macro_rules! audited {
    ($self:ident, $op:ident, |$txn:ident| $body:expr) => {{
        let $txn = $self.db.begin().await?;
        let (value, entry) = $body;
        record(&$txn, $op, &entry, $self.clock.now()).await?;
        $txn.commit().await?;
        Ok(value)
    }};
}

impl PlatformService {
    pub async fn operator_create_tenant(
        &self,
        op: &OperatorContext,
        new: NewTenant,
    ) -> PlatformResult<TenantView> {
        audited!(self, op, |txn| {
            let t = self.create_tenant_in(&txn, new).await?;
            let entry = AuditEntry::new(
                OperatorAction::TenantCreated,
                AuditTargetKind::Tenant,
                t.id.as_uuid(),
            )
            .scope(Some(t.id.as_uuid()), None, None)
            .detail(json!({ "classification": t.classification.as_str() }));
            (t, entry)
        })
    }

    pub async fn operator_set_tenant_status(
        &self,
        op: &OperatorContext,
        id: TenantId,
        status: TenantStatus,
    ) -> PlatformResult<TenantView> {
        audited!(self, op, |txn| {
            let t = self.set_tenant_status_in(&txn, id, status).await?;
            let entry = AuditEntry::new(
                OperatorAction::TenantStatusSet,
                AuditTargetKind::Tenant,
                t.id.as_uuid(),
            )
            .scope(Some(t.id.as_uuid()), None, None)
            .detail(json!({ "status": status.as_str() }));
            (t, entry)
        })
    }

    pub async fn operator_create_application(
        &self,
        op: &OperatorContext,
        tenant_id: TenantId,
        new: NewApplication,
    ) -> PlatformResult<ApplicationView> {
        audited!(self, op, |txn| {
            let a = self.create_application_in(&txn, tenant_id, new).await?;
            let entry = AuditEntry::new(
                OperatorAction::ApplicationCreated,
                AuditTargetKind::Application,
                a.id.as_uuid(),
            )
            .scope(Some(a.tenant_id.as_uuid()), Some(a.id.as_uuid()), None);
            (a, entry)
        })
    }

    pub async fn operator_set_application_status(
        &self,
        op: &OperatorContext,
        id: ApplicationId,
        status: ApplicationStatus,
    ) -> PlatformResult<ApplicationView> {
        audited!(self, op, |txn| {
            let a = self.set_application_status_in(&txn, id, status).await?;
            let entry = AuditEntry::new(
                OperatorAction::ApplicationStatusSet,
                AuditTargetKind::Application,
                a.id.as_uuid(),
            )
            .scope(Some(a.tenant_id.as_uuid()), Some(a.id.as_uuid()), None)
            .detail(json!({ "status": status.as_str() }));
            (a, entry)
        })
    }

    pub async fn operator_create_environment(
        &self,
        op: &OperatorContext,
        application_id: ApplicationId,
        kind: EnvironmentKind,
    ) -> PlatformResult<EnvironmentView> {
        audited!(self, op, |txn| {
            let e = self
                .create_environment_in(&txn, application_id, kind)
                .await?;
            let entry = environment_entry(OperatorAction::EnvironmentCreated, &e)
                .detail(json!({ "kind": kind.as_str() }));
            (e, entry)
        })
    }

    pub async fn operator_set_environment_status(
        &self,
        op: &OperatorContext,
        id: EnvironmentId,
        status: EnvironmentStatus,
    ) -> PlatformResult<EnvironmentView> {
        audited!(self, op, |txn| {
            let e = self.set_environment_status_in(&txn, id, status).await?;
            let entry = environment_entry(OperatorAction::EnvironmentStatusSet, &e)
                .detail(json!({ "status": status.as_str() }));
            (e, entry)
        })
    }

    pub async fn operator_set_live_telecom_allowed(
        &self,
        op: &OperatorContext,
        id: EnvironmentId,
        allowed: bool,
    ) -> PlatformResult<EnvironmentView> {
        audited!(self, op, |txn| {
            let e = self.set_live_telecom_allowed_in(&txn, id, allowed).await?;
            let entry = environment_entry(OperatorAction::EnvironmentLiveTelecomSet, &e)
                .detail(json!({ "liveTelecomAllowed": allowed }));
            (e, entry)
        })
    }

    pub async fn operator_create_service_account(
        &self,
        op: &OperatorContext,
        environment_id: EnvironmentId,
        new: NewServiceAccount,
    ) -> PlatformResult<ServiceAccountView> {
        audited!(self, op, |txn| {
            let sa = self
                .create_service_account_in(&txn, environment_id, new)
                .await?;
            let entry = service_account_entry(OperatorAction::ServiceAccountCreated, &sa);
            (sa, entry)
        })
    }

    pub async fn operator_set_service_account_status(
        &self,
        op: &OperatorContext,
        id: ServiceAccountId,
        status: ServiceAccountStatus,
    ) -> PlatformResult<ServiceAccountView> {
        audited!(self, op, |txn| {
            let sa = self.set_service_account_status_in(&txn, id, status).await?;
            let entry = service_account_entry(OperatorAction::ServiceAccountStatusSet, &sa)
                .detail(json!({ "status": status.as_str() }));
            (sa, entry)
        })
    }

    /// Issues a credential (audited). The raw secret is in the result only: the audit
    /// row carries the credential id, public id and scope names.
    pub async fn operator_issue_credential(
        &self,
        op: &OperatorContext,
        service_account_id: ServiceAccountId,
        request: IssueCredential,
    ) -> PlatformResult<IssuedCredential> {
        audited!(self, op, |txn| {
            let issued = self
                .issue_credential_in(&txn, service_account_id, request)
                .await?;
            let entry = credential_entry(OperatorAction::CredentialCreated, &issued.credential);
            (issued, entry)
        })
    }

    pub async fn operator_rotate_credential(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
        request: RotateCredential,
    ) -> PlatformResult<IssuedCredential> {
        audited!(self, op, |txn| {
            let issued = self
                .rotate_credential_in(&txn, credential_id, request)
                .await?;
            let mut entry = credential_entry(OperatorAction::CredentialRotated, &issued.credential);
            entry.target_id = Some(credential_id.as_uuid());
            entry.detail["successorCredentialId"] = json!(issued.credential.id);
            (issued, entry)
        })
    }

    /// Suspends a credential (audited, reversible; Part VI §54).
    pub async fn operator_suspend_credential(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
        reason_code: &str,
    ) -> PlatformResult<CredentialView> {
        audited!(self, op, |txn| {
            let (view, _) = self
                .suspend_credential_in(&txn, credential_id, op.operator(), reason_code)
                .await?;
            let mut entry = credential_entry(OperatorAction::CredentialSuspended, &view);
            entry.detail["reasonCode"] = json!(reason_code);
            (view, entry)
        })
    }

    /// Lifts a credential suspension (audited).
    pub async fn operator_resume_credential(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
    ) -> PlatformResult<CredentialView> {
        audited!(self, op, |txn| {
            let (view, _) = self.resume_credential_in(&txn, credential_id).await?;
            let entry = credential_entry(OperatorAction::CredentialResumed, &view);
            (view, entry)
        })
    }

    /// Activates the PRODUCTION environment of an application for an approved Developer
    /// Platform production-access request (audited, Part VI §55): creates it when absent
    /// and records the approval reference. An application is activated at most once; an
    /// already activated environment is returned unchanged (idempotent on the reference).
    pub async fn operator_activate_production_environment(
        &self,
        op: &OperatorContext,
        application_id: ApplicationId,
        approval_reference: &str,
    ) -> PlatformResult<EnvironmentView> {
        let valid = (1..=128).contains(&approval_reference.len())
            && approval_reference.as_bytes()[0].is_ascii_alphanumeric()
            && approval_reference
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_:-".contains(&b));
        if !valid {
            return Err(PlatformError::Validation(
                "approval_reference is malformed".to_string(),
            ));
        }
        if let Some((environment_id, reference)) =
            self.production_activation(application_id).await?
        {
            if reference != approval_reference {
                return Err(PlatformError::Conflict(
                    "the application's PRODUCTION environment was activated by another approval"
                        .to_string(),
                ));
            }
            return self.environment(environment_id).await;
        }
        let existing = match self
            .environment_by_kind(application_id, EnvironmentKind::Production)
            .await
        {
            Ok(e) => Some(e),
            Err(PlatformError::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        audited!(self, op, |txn| {
            let e = match existing {
                Some(e) => e,
                None => {
                    self.create_environment_in(&txn, application_id, EnvironmentKind::Production)
                        .await?
                }
            };
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO platform_production_activations (environment_id, tenant_id, \
                   application_id, approval_reference, activated_by, activated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                [
                    e.id.as_uuid().into(),
                    e.tenant_id.as_uuid().into(),
                    e.application_id.as_uuid().into(),
                    approval_reference.into(),
                    op.operator().into(),
                    self.clock.now().into(),
                ],
            ))
            .await?;
            let entry = environment_entry(OperatorAction::ProductionEnvironmentActivated, &e)
                .detail(json!({ "approvalReference": approval_reference }));
            (e, entry)
        })
    }

    /// `(environment id, approval reference)` of an application's activated PRODUCTION
    /// environment.
    pub async fn production_activation(
        &self,
        application_id: ApplicationId,
    ) -> PlatformResult<Option<(EnvironmentId, String)>> {
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT environment_id, approval_reference FROM platform_production_activations \
                 WHERE application_id = $1",
                [application_id.as_uuid().into()],
            ))
            .await?;
        Ok(match row {
            Some(r) => Some((
                EnvironmentId::from_uuid(r.try_get("", "environment_id")?),
                r.try_get("", "approval_reference")?,
            )),
            None => None,
        })
    }

    /// Whether `environment_id` is a PRODUCTION environment activated by an approval.
    pub async fn is_activated_production(
        &self,
        environment_id: EnvironmentId,
    ) -> PlatformResult<bool> {
        Ok(self
            .db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM platform_production_activations WHERE environment_id = $1",
                [environment_id.as_uuid().into()],
            ))
            .await?
            .is_some())
    }

    pub async fn operator_revoke_credential(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
    ) -> PlatformResult<CredentialView> {
        audited!(self, op, |txn| {
            let view = self.revoke_credential_in(&txn, credential_id).await?;
            let entry = credential_entry(OperatorAction::CredentialRevoked, &view);
            (view, entry)
        })
    }

    pub async fn operator_grant_application_scope(
        &self,
        op: &OperatorContext,
        application_id: ApplicationId,
        scope: &str,
        environment_kinds: Option<&[EnvironmentKind]>,
    ) -> PlatformResult<ApplicationScopeView> {
        let application = self.application(application_id).await?;
        audited!(self, op, |txn| {
            let view = self
                .grant_application_scope_in(&txn, application_id, scope, environment_kinds)
                .await?;
            let entry = AuditEntry::new(
                OperatorAction::ScopeGranted,
                AuditTargetKind::Application,
                application_id.as_uuid(),
            )
            .scope(
                Some(application.tenant_id.as_uuid()),
                Some(application_id.as_uuid()),
                None,
            )
            .detail(json!({
                "scope": view.scope,
                "environmentKinds": view.allowed_environment_kinds,
            }));
            (view, entry)
        })
    }

    pub async fn operator_revoke_application_scope(
        &self,
        op: &OperatorContext,
        application_id: ApplicationId,
        scope: &str,
    ) -> PlatformResult<bool> {
        let application = self.application(application_id).await?;
        // Only a registry name is echoed into the audit row (never arbitrary input).
        let name = self.registry.definition(scope).map(|d| d.name);
        audited!(self, op, |txn| {
            let removed = self
                .revoke_application_scope_in(&txn, application_id, scope)
                .await?;
            let entry = AuditEntry::new(
                OperatorAction::ScopeRevoked,
                AuditTargetKind::Application,
                application_id.as_uuid(),
            )
            .scope(
                Some(application.tenant_id.as_uuid()),
                Some(application_id.as_uuid()),
                None,
            )
            .detail(json!({ "scope": name.unwrap_or("UNKNOWN"), "removed": removed }));
            (removed, entry)
        })
    }

    pub async fn operator_register_principal(
        &self,
        op: &OperatorContext,
        environment_id: EnvironmentId,
        new: NewPrincipal,
    ) -> PlatformResult<(PrincipalView, bool)> {
        audited!(self, op, |txn| {
            let (p, created) = self
                .register_principal_in(&txn, environment_id, new)
                .await?;
            let entry = principal_entry(OperatorAction::PrincipalRegistered, &p)
                .detail(json!({ "kind": p.kind.as_str(), "created": created }));
            ((p, created), entry)
        })
    }

    pub async fn operator_set_principal_status(
        &self,
        op: &OperatorContext,
        id: PrincipalId,
        status: PrincipalStatus,
    ) -> PlatformResult<PrincipalView> {
        audited!(self, op, |txn| {
            let p = self.set_principal_status_in(&txn, id, status).await?;
            let entry = principal_entry(OperatorAction::PrincipalStatusSet, &p)
                .detail(json!({ "status": status.as_str() }));
            (p, entry)
        })
    }

    /// Grants a privileged operator scope (`numbers.admin`) to an application of a
    /// FIRST_PARTY tenant (audited as PRIVILEGED_SCOPE_GRANTED).
    pub async fn operator_grant_privileged_scope(
        &self,
        op: &OperatorContext,
        application_id: ApplicationId,
        scope: &str,
        environment_kinds: Option<&[EnvironmentKind]>,
    ) -> PlatformResult<ApplicationScopeView> {
        let application = self.application(application_id).await?;
        audited!(self, op, |txn| {
            let view = self
                .grant_privileged_scope_in(&txn, application_id, scope, environment_kinds)
                .await?;
            let entry = AuditEntry::new(
                OperatorAction::PrivilegedScopeGranted,
                AuditTargetKind::Application,
                application_id.as_uuid(),
            )
            .scope(
                Some(application.tenant_id.as_uuid()),
                Some(application_id.as_uuid()),
                None,
            )
            .detail(json!({
                "scope": view.scope,
                "environmentKinds": view.allowed_environment_kinds,
            }));
            (view, entry)
        })
    }

    /// Replaces the authorized-principal restriction of a credential (an empty list
    /// removes it).
    pub async fn operator_restrict_credential_principals(
        &self,
        op: &OperatorContext,
        credential_id: CredentialId,
        principals: &[PrincipalId],
    ) -> PlatformResult<Vec<PrincipalId>> {
        audited!(self, op, |txn| {
            let (credential, restricted) = self
                .restrict_credential_principals_in(&txn, credential_id, principals)
                .await?;
            let mut entry =
                credential_entry(OperatorAction::CredentialPrincipalsRestricted, &credential);
            entry.detail = json!({ "principalCount": restricted.len() });
            (restricted, entry)
        })
    }

    /// Tombstones a principal (audited as PRINCIPAL_TOMBSTONED).
    pub async fn operator_tombstone_principal(
        &self,
        op: &OperatorContext,
        id: PrincipalId,
    ) -> PlatformResult<super::principals::TombstonedPrincipal> {
        audited!(self, op, |txn| {
            let t = self.tombstone_principal_in(&txn, id).await?;
            let entry = principal_entry(OperatorAction::PrincipalTombstoned, &t.principal)
                .detail(json!({ "alreadyTombstoned": t.already_tombstoned }));
            (t, entry)
        })
    }
}

fn environment_entry(action: OperatorAction, e: &EnvironmentView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Environment, e.id.as_uuid()).scope(
        Some(e.tenant_id.as_uuid()),
        Some(e.application_id.as_uuid()),
        Some(e.id.as_uuid()),
    )
}

fn service_account_entry(action: OperatorAction, sa: &ServiceAccountView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::ServiceAccount, sa.id.as_uuid()).scope(
        Some(sa.tenant_id.as_uuid()),
        Some(sa.application_id.as_uuid()),
        Some(sa.environment_id.as_uuid()),
    )
}

fn credential_entry(action: OperatorAction, c: &CredentialView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Credential, c.id.as_uuid())
        .scope(
            Some(c.tenant_id.as_uuid()),
            Some(c.application_id.as_uuid()),
            Some(c.environment_id.as_uuid()),
        )
        .detail(json!({
            "publicId": c.public_id,
            "serviceAccountId": c.service_account_id,
            "scopes": c.scopes,
        }))
}

fn principal_entry(action: OperatorAction, p: &PrincipalView) -> AuditEntry {
    AuditEntry::new(action, AuditTargetKind::Principal, p.id.as_uuid()).scope(
        Some(p.tenant_id.as_uuid()),
        Some(p.application_id.as_uuid()),
        Some(p.environment_id.as_uuid()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_ids_are_bounded_opaque_handles() {
        for ok in ["ops.alice", "a", "svc-ops@example", "OPS_1"] {
            let op = OperatorContext::new(ok).unwrap();
            assert_eq!(op.operator(), ok);
            let c = op.correlation_id();
            assert!(c.len() >= 8 && c.len() <= 64);
            assert!(c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        }
        for bad in [
            "",
            " ",
            "-ops",
            "has space",
            "+2348012345678",
            &"a".repeat(101),
        ] {
            assert!(OperatorContext::new(bad).is_err(), "{bad}");
        }
        assert_ne!(
            OperatorContext::new("ops").unwrap().correlation_id(),
            OperatorContext::new("ops").unwrap().correlation_id()
        );
    }

    #[test]
    fn the_action_vocabulary_is_the_tables() {
        // Kept in step with migrations 000003 / 000004 (an unknown code is refused there).
        assert_eq!(OperatorAction::ALL.len(), 23);
        assert_eq!(AuditTargetKind::ALL.len(), 7);
    }
}
