//! Automatic organization service bindings (ADR 0058 Amendment A2).
//!
//! The Organizations Service provisions, for each first-party organization, ONE service
//! account and ONE credential per consuming service (`calling`, `messaging`) INSIDE that
//! organization's own FIRST_PARTY triple (`platform_organization_scopes`). Platform Access
//! stays the only identity authority: the triple comes from the scope registry, never from
//! the caller; the scope set per service is closed and fixed here; the raw credential is
//! dropped (zeroized) the moment it is issued — it is never returned, logged or stored
//! (the credential table keeps only its verifier). Consumers name the credential by id and
//! Platform Access re-verifies it on every use.
//!
//! * [`PlatformService::ensure_organization_binding`] — create or return (idempotent on
//!   `(organization_ref, capability, service)`, serialized by an advisory lock). An
//!   existing binding whose credential no longer authenticates (expired, revoked,
//!   superseded) or expires within [`RENEW_WITHIN_DAYS`] days, or whose scopes drifted,
//!   gets a fresh credential for the same service account (the predecessor is revoked).
//! * [`PlatformService::organization_binding`] — read back with the server-side facts.
//! * [`PlatformService::rotate_organization_binding`] — forced rotation.
//! * [`PlatformService::revoke_organization_binding`] — terminal revocation (archive).
//!
//! Every committed change writes the operator audit with the calling service as actor and
//! `detail.organizationRef` / `detail.service`.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    credentials::{CredentialEffectiveState, CredentialView, IssueCredential, RotateCredential},
    operator::{record, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    organization_scopes::OrganizationScope,
    scopes::allowed_scopes,
    service_accounts::{NewServiceAccount, ServiceAccountView},
    PlatformError, PlatformResult, PlatformService,
};
use crate::domain::{
    ApplicationStatus, CredentialId, EnvironmentStatus, ServiceAccountId, ServiceAccountStatus,
    TenantStatus,
};

/// A binding's credential is renewed on `PUT` when it expires within this many days.
pub const RENEW_WITHIN_DAYS: i64 = 30;

/// The closed vocabulary of bindable services: `(service, service account name, scopes)`.
pub const ORGANIZATION_BINDING_SERVICES: &[(&str, &str, &[&str])] = &[
    (
        "calling",
        "org-calling",
        &[
            "calls.read",
            "enterprise.config.read",
            "enterprise.config.write",
        ],
    ),
    ("messaging", "org-messaging", &["messages.read"]),
];

/// The service's `(name, service account name, required scopes)`; unknown → validation.
pub fn binding_service(
    value: &str,
) -> PlatformResult<(&'static str, &'static str, &'static [&'static str])> {
    ORGANIZATION_BINDING_SERVICES
        .iter()
        .find(|(s, _, _)| *s == value)
        .copied()
        .ok_or_else(|| PlatformError::Validation("unknown organization binding service".into()))
}

fn required_scopes(scopes: &[&str]) -> BTreeSet<String> {
    scopes.iter().map(|s| s.to_string()).collect()
}

/// One binding read back from the authority, with the facts the caller verifies.
#[derive(Debug, Clone)]
pub struct OrganizationBinding {
    pub organization_ref: Uuid,
    pub capability: String,
    pub service: String,
    pub scope: OrganizationScope,
    pub service_account: ServiceAccountView,
    pub credential: CredentialView,
    pub required_scopes: BTreeSet<String>,
    pub revision: i64,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrganizationBinding {
    fn credential_scopes(&self) -> BTreeSet<String> {
        self.credential.scopes.iter().cloned().collect()
    }

    /// Service account and credential belong to exactly the organization's verified
    /// triple, and the credential carries exactly the service's scopes.
    pub fn verified(&self) -> bool {
        let (t, a, e) = (
            self.scope.tenant.id,
            self.scope.application.id,
            self.scope.environment.id,
        );
        self.scope.verified()
            && self.service_account.tenant_id == t
            && self.service_account.application_id == a
            && self.service_account.environment_id == e
            && self.credential.tenant_id == t
            && self.credential.application_id == a
            && self.credential.environment_id == e
            && self.credential.service_account_id == self.service_account.id
            && self.credential_scopes() == self.required_scopes
    }

    /// The credential would authenticate now (credential state, service account and the
    /// whole triple ACTIVE).
    pub fn authenticates(&self) -> bool {
        self.revoked_at.is_none()
            && self.credential.effective_state.authenticates()
            && self.service_account.status == ServiceAccountStatus::Active
            && self.scope.active()
    }

    pub fn active(&self) -> bool {
        self.verified() && self.authenticates()
    }

    /// The wire view (`OrganizationServiceBinding`). Ids, statuses and scopes only; never
    /// a secret, verifier, public id or masked credential.
    pub fn to_json(&self) -> Value {
        json!({
            "organizationRef": self.organization_ref,
            "capability": self.capability,
            "service": self.service,
            "tenantId": self.scope.tenant.id,
            "applicationId": self.scope.application.id,
            "environmentId": self.scope.environment.id,
            "serviceAccountId": self.service_account.id,
            "credentialId": self.credential.id,
            "requiredScopes": self.required_scopes,
            "scopes": self.credential_scopes(),
            "serviceAccount": {
                "id": self.service_account.id,
                "status": self.service_account.status,
            },
            "credential": {
                "id": self.credential.id,
                "status": self.credential.effective_state,
                "expiresAt": self.credential.expires_at,
                "authenticates": self.authenticates(),
            },
            "revision": self.revision,
            "verified": self.verified(),
            "active": self.active(),
            "revokedAt": self.revoked_at,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        })
    }
}

struct BindingRow {
    service_account_id: Uuid,
    credential_id: Uuid,
    revision: i64,
    revoked_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

async fn binding_row<C: ConnectionTrait>(
    db: &C,
    organization_ref: Uuid,
    capability: &str,
    service: &str,
) -> PlatformResult<Option<BindingRow>> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT service_account_id, credential_id, revision, revoked_at, created_at, updated_at \
             FROM platform_organization_service_bindings \
             WHERE organization_ref = $1 AND capability = $2 AND service = $3",
            [organization_ref.into(), capability.into(), service.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => Some(BindingRow {
            service_account_id: r.try_get("", "service_account_id")?,
            credential_id: r.try_get("", "credential_id")?,
            revision: r.try_get("", "revision")?,
            revoked_at: r.try_get("", "revoked_at")?,
            created_at: r.try_get("", "created_at")?,
            updated_at: r.try_get("", "updated_at")?,
        }),
        None => None,
    })
}

/// What `ensure` did (for the HTTP status).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingOutcome {
    Created,
    Unchanged,
    Renewed,
}

fn scope_inactive(scope: &OrganizationScope) -> Option<&'static str> {
    if scope.tenant.status != TenantStatus::Active {
        Some("tenant")
    } else if scope.application.status != ApplicationStatus::Active {
        Some("application")
    } else if scope.environment.status != EnvironmentStatus::Active {
        Some("environment")
    } else {
        None
    }
}

impl PlatformService {
    async fn binding_scope(
        &self,
        organization_ref: Uuid,
        capability: &str,
    ) -> PlatformResult<OrganizationScope> {
        if organization_ref.is_nil() {
            return Err(PlatformError::Validation(
                "the organization reference must be a non-nil UUID".into(),
            ));
        }
        let scope = self
            .organization_scope(organization_ref, capability)
            .await?
            .ok_or(PlatformError::NotFound("organization scope"))?;
        if !scope.verified() {
            return Err(PlatformError::Conflict(
                "the organization scope does not verify".into(),
            ));
        }
        Ok(scope)
    }

    async fn assemble<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: OrganizationScope,
        service: &'static str,
        scopes: &[&str],
        row: BindingRow,
    ) -> PlatformResult<OrganizationBinding> {
        let service_account: ServiceAccountView =
            crate::models::service_accounts::Entity::find_by_id(row.service_account_id)
                .one(db)
                .await?
                .map(Into::into)
                .ok_or(PlatformError::NotFound("service account"))?;
        let credential = self
            .credential_in(db, CredentialId::from_uuid(row.credential_id))
            .await?;
        Ok(OrganizationBinding {
            organization_ref: scope.organization_ref,
            capability: scope.capability.clone(),
            service: service.to_string(),
            scope,
            service_account,
            credential,
            required_scopes: required_scopes(scopes),
            revision: row.revision,
            revoked_at: row.revoked_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    /// The organization's binding of `service`, `None` when never bound.
    pub async fn organization_binding(
        &self,
        organization_ref: Uuid,
        capability: &str,
        service: &str,
    ) -> PlatformResult<Option<OrganizationBinding>> {
        let (service, _, scopes) = binding_service(service)?;
        let scope = self.binding_scope(organization_ref, capability).await?;
        let Some(row) = binding_row(&self.db, organization_ref, &scope.capability, service).await?
        else {
            return Ok(None);
        };
        Ok(Some(
            self.assemble(&self.db, scope, service, scopes, row).await?,
        ))
    }

    async fn lock<C: ConnectionTrait>(
        &self,
        txn: &C,
        organization_ref: Uuid,
        capability: &str,
        service: &str,
    ) -> PlatformResult<()> {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [
                format!("platform-organization-binding:{organization_ref}:{capability}:{service}")
                    .into(),
            ],
        ))
        .await?;
        Ok(())
    }

    /// Issues a fresh credential with exactly `scopes` for the binding's service account
    /// inside `txn`; the raw secret is dropped (zeroized) here.
    async fn issue_binding_credential<C: ConnectionTrait + TransactionTrait>(
        &self,
        txn: &C,
        service_account: ServiceAccountId,
        service: &str,
        scopes: &[&str],
    ) -> PlatformResult<CredentialView> {
        let issued = self
            .issue_credential_in(
                txn,
                service_account,
                IssueCredential {
                    scopes: scopes.iter().map(|s| s.to_string()).collect(),
                    label: Some(format!("organization {service} binding")),
                    not_before: None,
                    expires_at: None,
                },
            )
            .await?;
        let credential = issued.credential.clone();
        drop(issued); // the raw credential is never returned, logged or stored
        Ok(credential)
    }

    /// Grants the service's scopes to the organization's application (idempotent) and
    /// audits each newly allowed scope.
    async fn grant_binding_scopes<C: ConnectionTrait>(
        &self,
        txn: &C,
        op: &OperatorContext,
        scope: &OrganizationScope,
        service: &str,
        scopes: &[&str],
        now: DateTime<Utc>,
    ) -> PlatformResult<()> {
        let allowed = allowed_scopes(txn, scope.application.id).await?;
        for name in scopes {
            if allowed.contains_key(*name) {
                continue;
            }
            self.grant_application_scope_in(txn, scope.application.id, name, None)
                .await?;
            let (t, a) = (scope.tenant.id.as_uuid(), scope.application.id.as_uuid());
            record(
                txn,
                op,
                &AuditEntry::new(
                    OperatorAction::ScopeGranted,
                    AuditTargetKind::Application,
                    a,
                )
                .scope(Some(t), Some(a), None)
                .detail(self.binding_detail(
                    scope,
                    service,
                    json!({ "scope": name }),
                )),
                now,
            )
            .await?;
        }
        Ok(())
    }

    fn binding_detail(&self, scope: &OrganizationScope, service: &str, extra: Value) -> Value {
        let mut detail = json!({
            "organizationRef": scope.organization_ref,
            "capability": scope.capability,
            "service": service,
        });
        if let (Some(d), Value::Object(e)) = (detail.as_object_mut(), extra) {
            d.extend(e);
        }
        detail
    }

    #[allow(clippy::too_many_arguments)]
    async fn audit_credential<C: ConnectionTrait>(
        &self,
        txn: &C,
        op: &OperatorContext,
        scope: &OrganizationScope,
        service: &str,
        action: OperatorAction,
        credential: Uuid,
        extra: Value,
        now: DateTime<Utc>,
    ) -> PlatformResult<()> {
        let (t, a, e) = (
            scope.tenant.id.as_uuid(),
            scope.application.id.as_uuid(),
            scope.environment.id.as_uuid(),
        );
        record(
            txn,
            op,
            &AuditEntry::new(action, AuditTargetKind::Credential, credential)
                .scope(Some(t), Some(a), Some(e))
                .detail(self.binding_detail(scope, service, extra)),
            now,
        )
        .await?;
        Ok(())
    }

    /// Replaces the binding's credential inside `txn`: rotation (overlap 0) of an ACTIVE
    /// credential, a fresh issue otherwise; the predecessor is revoked either way. A
    /// suspended credential (Part VI §54 operator decision) is never bypassed.
    #[allow(clippy::too_many_arguments)]
    async fn replace_binding_credential<C: ConnectionTrait + TransactionTrait>(
        &self,
        txn: &C,
        op: &OperatorContext,
        scope: &OrganizationScope,
        service: &str,
        scopes: &[&str],
        current: &CredentialView,
        reason: &str,
        now: DateTime<Utc>,
    ) -> PlatformResult<CredentialView> {
        if current.effective_state == CredentialEffectiveState::Suspended {
            return Err(PlatformError::InvalidState(
                "the binding's credential is suspended; resume it before renewing".into(),
            ));
        }
        let successor = if current.effective_state == CredentialEffectiveState::Active {
            let issued = self
                .rotate_credential_in(
                    txn,
                    current.id,
                    RotateCredential {
                        overlap: Some(Duration::zero()),
                        scopes: Some(scopes.iter().map(|s| s.to_string()).collect()),
                        label: None,
                        expires_at: None,
                    },
                )
                .await?;
            let credential = issued.credential.clone();
            drop(issued); // the raw credential is never returned, logged or stored
            self.audit_credential(
                txn,
                op,
                scope,
                service,
                OperatorAction::CredentialRotated,
                credential.id.as_uuid(),
                json!({ "rotatedFrom": current.id, "reason": reason }),
                now,
            )
            .await?;
            credential
        } else {
            let credential = self
                .issue_binding_credential(txn, current.service_account_id, service, scopes)
                .await?;
            self.audit_credential(
                txn,
                op,
                scope,
                service,
                OperatorAction::CredentialCreated,
                credential.id.as_uuid(),
                json!({ "replaces": current.id, "reason": reason }),
                now,
            )
            .await?;
            credential
        };
        if current.status != crate::domain::CredentialStatus::Revoked {
            self.revoke_credential_in(txn, current.id).await?;
            self.audit_credential(
                txn,
                op,
                scope,
                service,
                OperatorAction::CredentialRevoked,
                current.id.as_uuid(),
                json!({ "replacedBy": successor.id, "reason": reason }),
                now,
            )
            .await?;
        }
        Ok(successor)
    }

    async fn record_successor<C: ConnectionTrait>(
        &self,
        txn: &C,
        organization_ref: Uuid,
        capability: &str,
        service: &str,
        credential: CredentialId,
        now: DateTime<Utc>,
    ) -> PlatformResult<()> {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE platform_organization_service_bindings \
             SET credential_id = $4, revision = revision + 1, updated_at = $5 \
             WHERE organization_ref = $1 AND capability = $2 AND service = $3",
            [
                organization_ref.into(),
                capability.into(),
                service.into(),
                credential.as_uuid().into(),
                now.into(),
            ],
        ))
        .await?;
        Ok(())
    }

    /// Creates (first call) or returns (later calls) the organization's binding of
    /// `service`, renewing its credential when it no longer authenticates, expires within
    /// [`RENEW_WITHIN_DAYS`] days or carries other scopes than the service requires.
    pub async fn ensure_organization_binding(
        &self,
        op: &OperatorContext,
        requested_by: &str,
        organization_ref: Uuid,
        capability: &str,
        service: &str,
    ) -> PlatformResult<(OrganizationBinding, BindingOutcome)> {
        let (service, account_name, scopes) = binding_service(service)?;
        let scope = self.binding_scope(organization_ref, capability).await?;
        let cap = scope.capability.clone();
        if let Some(what) = scope_inactive(&scope) {
            return Err(PlatformError::InvalidState(format!(
                "the organization {what} is not ACTIVE; bindings cannot be provisioned"
            )));
        }
        let txn = self.db.begin().await?;
        self.lock(&txn, organization_ref, &cap, service).await?;
        let now = self.clock.now();
        let outcome = match binding_row(&txn, organization_ref, &cap, service).await? {
            Some(row) => {
                if row.revoked_at.is_some() {
                    return Err(PlatformError::InvalidState(
                        "the organization binding was revoked; it cannot be re-provisioned".into(),
                    ));
                }
                let current = self
                    .assemble(&txn, scope.clone(), service, scopes, row)
                    .await?;
                if current.service_account.status != ServiceAccountStatus::Active {
                    return Err(PlatformError::InvalidState(
                        "the binding's service account is not ACTIVE".into(),
                    ));
                }
                let fresh = current.credential.effective_state == CredentialEffectiveState::Active
                    && current.credential.expires_at > now + Duration::days(RENEW_WITHIN_DAYS);
                if fresh && current.verified() {
                    BindingOutcome::Unchanged
                } else {
                    let reason = if !fresh { "RENEWAL" } else { "SCOPE_DRIFT" };
                    self.grant_binding_scopes(&txn, op, &scope, service, scopes, now)
                        .await?;
                    let successor = self
                        .replace_binding_credential(
                            &txn,
                            op,
                            &scope,
                            service,
                            scopes,
                            &current.credential,
                            reason,
                            now,
                        )
                        .await?;
                    self.record_successor(&txn, organization_ref, &cap, service, successor.id, now)
                        .await?;
                    BindingOutcome::Renewed
                }
            }
            None => {
                // A service account with the reserved name outside the registry is never
                // adopted (it may have been created by an operator for another purpose).
                let taken = txn
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT 1 AS x FROM platform_service_accounts WHERE environment_id = $1 AND name = $2",
                        [scope.environment.id.as_uuid().into(), account_name.into()],
                    ))
                    .await?
                    .is_some();
                if taken {
                    return Err(PlatformError::Conflict(format!(
                        "a service account named `{account_name}` exists in the organization environment outside the binding registry"
                    )));
                }
                self.grant_binding_scopes(&txn, op, &scope, service, scopes, now)
                    .await?;
                let account = self
                    .create_service_account_in(
                        &txn,
                        scope.environment.id,
                        NewServiceAccount {
                            name: account_name.to_string(),
                            description: Some(format!(
                                "CF Meet organization {service} binding (managed by Organizations)"
                            )),
                        },
                    )
                    .await?;
                let (t, a, e) = (
                    scope.tenant.id.as_uuid(),
                    scope.application.id.as_uuid(),
                    scope.environment.id.as_uuid(),
                );
                record(
                    &txn,
                    op,
                    &AuditEntry::new(
                        OperatorAction::ServiceAccountCreated,
                        AuditTargetKind::ServiceAccount,
                        account.id.as_uuid(),
                    )
                    .scope(Some(t), Some(a), Some(e))
                    .detail(self.binding_detail(&scope, service, json!({}))),
                    now,
                )
                .await?;
                let credential = self
                    .issue_binding_credential(&txn, account.id, service, scopes)
                    .await?;
                self.audit_credential(
                    &txn,
                    op,
                    &scope,
                    service,
                    OperatorAction::CredentialCreated,
                    credential.id.as_uuid(),
                    json!({ "scopes": scopes }),
                    now,
                )
                .await?;
                txn.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO platform_organization_service_bindings (organization_ref, capability, \
                       service, tenant_id, application_id, environment_id, service_account_id, \
                       credential_id, revision, revoked_at, requested_by, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1, NULL, $9, $10, $10)",
                    [
                        organization_ref.into(),
                        cap.as_str().into(),
                        service.into(),
                        t.into(),
                        a.into(),
                        e.into(),
                        account.id.as_uuid().into(),
                        credential.id.as_uuid().into(),
                        requested_by.into(),
                        now.into(),
                    ],
                ))
                .await?;
                BindingOutcome::Created
            }
        };
        txn.commit().await?;
        if outcome != BindingOutcome::Unchanged {
            tracing::info!(organization_ref = %organization_ref, service, ?outcome, "organization service binding provisioned");
        }
        let binding = self
            .organization_binding(organization_ref, &cap, service)
            .await?
            .ok_or(PlatformError::NotFound("organization binding"))?;
        Ok((binding, outcome))
    }

    /// Forced rotation: a successor credential with the service's scopes; the predecessor
    /// stops authenticating immediately.
    pub async fn rotate_organization_binding(
        &self,
        op: &OperatorContext,
        organization_ref: Uuid,
        capability: &str,
        service: &str,
    ) -> PlatformResult<OrganizationBinding> {
        let (service, _, scopes) = binding_service(service)?;
        let scope = self.binding_scope(organization_ref, capability).await?;
        let cap = scope.capability.clone();
        let txn = self.db.begin().await?;
        self.lock(&txn, organization_ref, &cap, service).await?;
        let row = binding_row(&txn, organization_ref, &cap, service)
            .await?
            .ok_or(PlatformError::NotFound("organization binding"))?;
        if row.revoked_at.is_some() {
            return Err(PlatformError::InvalidState(
                "the organization binding was revoked".into(),
            ));
        }
        if let Some(what) = scope_inactive(&scope) {
            return Err(PlatformError::InvalidState(format!(
                "the organization {what} is not ACTIVE; the binding cannot be rotated"
            )));
        }
        let current = self
            .assemble(&txn, scope.clone(), service, scopes, row)
            .await?;
        let now = self.clock.now();
        self.grant_binding_scopes(&txn, op, &scope, service, scopes, now)
            .await?;
        let successor = self
            .replace_binding_credential(
                &txn,
                op,
                &scope,
                service,
                scopes,
                &current.credential,
                "ROTATION",
                now,
            )
            .await?;
        self.record_successor(&txn, organization_ref, &cap, service, successor.id, now)
            .await?;
        txn.commit().await?;
        tracing::info!(organization_ref = %organization_ref, service, "organization service binding rotated");
        self.organization_binding(organization_ref, &cap, service)
            .await?
            .ok_or(PlatformError::NotFound("organization binding"))
    }

    /// Terminal revocation (organization archived): the credential is revoked and the
    /// service account REVOKED. Idempotent.
    pub async fn revoke_organization_binding(
        &self,
        op: &OperatorContext,
        organization_ref: Uuid,
        capability: &str,
        service: &str,
    ) -> PlatformResult<OrganizationBinding> {
        let (service, _, scopes) = binding_service(service)?;
        let scope = self.binding_scope(organization_ref, capability).await?;
        let cap = scope.capability.clone();
        let txn = self.db.begin().await?;
        self.lock(&txn, organization_ref, &cap, service).await?;
        let row = binding_row(&txn, organization_ref, &cap, service)
            .await?
            .ok_or(PlatformError::NotFound("organization binding"))?;
        if row.revoked_at.is_none() {
            let now = self.clock.now();
            let current = self
                .assemble(&txn, scope.clone(), service, scopes, row)
                .await?;
            if current.credential.status != crate::domain::CredentialStatus::Revoked {
                self.revoke_credential_in(&txn, current.credential.id)
                    .await?;
                self.audit_credential(
                    &txn,
                    op,
                    &scope,
                    service,
                    OperatorAction::CredentialRevoked,
                    current.credential.id.as_uuid(),
                    json!({ "reason": "BINDING_REVOKED" }),
                    now,
                )
                .await?;
            }
            if current.service_account.status != ServiceAccountStatus::Revoked {
                self.set_service_account_status_in(
                    &txn,
                    current.service_account.id,
                    ServiceAccountStatus::Revoked,
                )
                .await?;
                let (t, a, e) = (
                    scope.tenant.id.as_uuid(),
                    scope.application.id.as_uuid(),
                    scope.environment.id.as_uuid(),
                );
                record(
                    &txn,
                    op,
                    &AuditEntry::new(
                        OperatorAction::ServiceAccountStatusSet,
                        AuditTargetKind::ServiceAccount,
                        current.service_account.id.as_uuid(),
                    )
                    .scope(Some(t), Some(a), Some(e))
                    .detail(self.binding_detail(
                        &scope,
                        service,
                        json!({ "status": "REVOKED", "reason": "BINDING_REVOKED" }),
                    )),
                    now,
                )
                .await?;
            }
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE platform_organization_service_bindings \
                 SET revoked_at = $4, revision = revision + 1, updated_at = $4 \
                 WHERE organization_ref = $1 AND capability = $2 AND service = $3",
                [
                    organization_ref.into(),
                    cap.as_str().into(),
                    service.into(),
                    now.into(),
                ],
            ))
            .await?;
            tracing::info!(organization_ref = %organization_ref, service, "organization service binding revoked");
        }
        txn.commit().await?;
        self.organization_binding(organization_ref, &cap, service)
            .await?
            .ok_or(PlatformError::NotFound("organization binding"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_vocabulary_is_closed_and_names_are_valid_slugs() {
        assert_eq!(binding_service("calling").unwrap().1, "org-calling");
        assert_eq!(
            binding_service("calling").unwrap().2,
            &[
                "calls.read",
                "enterprise.config.read",
                "enterprise.config.write"
            ]
        );
        assert_eq!(binding_service("messaging").unwrap().2, &["messages.read"]);
        for (_, name, _) in ORGANIZATION_BINDING_SERVICES {
            assert!(crate::domain::Slug::parse(name).is_ok(), "{name}");
        }
        for bad in ["", "Calling", "telephony", "calling "] {
            assert!(binding_service(bad).is_err(), "{bad}");
        }
    }
}
