//! Governed FIRST_PARTY organization scopes (Blueprint v1.0 Section 7, ADR 0058).
//!
//! A first-party business organization of the Organizations Service maps to ONE dedicated
//! FIRST_PARTY tenant with ONE first-party application and ONE PRODUCTION-kind environment
//! (ADR 0056 §4). Platform Access stays the only tenant authority: it creates that triple
//! here, keyed by the OPAQUE organization id, and Organizations stores only the ids and its
//! own verification evidence.
//!
//! * [`PlatformService::provision_organization_scope`] is idempotent on
//!   `(organization_ref, capability)`: the first call creates the tenant, application,
//!   environment and the `platform_organization_scopes` row in ONE transaction (all or
//!   nothing — there is never a half-created triple to resume), concurrent calls serialize
//!   on an advisory lock, and every later call returns the same triple.
//! * [`PlatformService::organization_scope`] reads the triple back with the server-side
//!   facts the caller verifies (classification, kind, ownership chain, statuses).
//! * [`PlatformService::set_organization_scope_status`] suspends / resumes the TENANT
//!   (every subordinate credential then fails authentication immediately).
//!
//! No credential, verifier, service account, principal or scope grant is created here;
//! the application starts with no allowed scope. Every committed change writes the
//! operator audit (`platform_access_audit`) with the organization id as an opaque
//! reference (`detail.organizationRef`).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    applications::{ApplicationView, NewApplication},
    environments::EnvironmentView,
    operator::{record, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    tenants::{NewTenant, TenantView},
    PlatformError, PlatformResult, PlatformService,
};
use crate::domain::{
    ApplicationId, ApplicationStatus, EnvironmentId, EnvironmentKind, EnvironmentStatus,
    TenantClassification, TenantId, TenantStatus,
};

/// The organization-scope capabilities Platform Access provisions. ADR 0056 §4: one
/// triple per organization (`primary`) carries Calling, Messaging and every other
/// organization-scoped technical resource.
pub const ORGANIZATION_SCOPE_CAPABILITIES: &[&str] = &["primary"];

/// The reserved slug of an organization's dedicated tenant (`org-<32 hex>`).
pub fn organization_tenant_slug(organization_ref: Uuid) -> String {
    format!("org-{}", organization_ref.simple())
}

/// Slug of the organization's first-party application (unique per tenant).
pub const ORGANIZATION_APPLICATION_SLUG: &str = "organization";

/// The provisioned scope of one organization, read back from the authority.
#[derive(Debug, Clone)]
pub struct OrganizationScope {
    pub organization_ref: Uuid,
    pub capability: String,
    pub tenant: TenantView,
    pub application: ApplicationView,
    pub environment: EnvironmentView,
    pub requested_by: String,
    pub created_at: DateTime<Utc>,
}

impl OrganizationScope {
    /// Whether the triple is exactly what Section 7 provisions: a FIRST_PARTY tenant with
    /// the organization's reserved slug, owning the application, owning a PRODUCTION
    /// environment.
    pub fn verified(&self) -> bool {
        self.tenant.classification == TenantClassification::FirstParty
            && self.tenant.slug == organization_tenant_slug(self.organization_ref)
            && self.application.tenant_id == self.tenant.id
            && self.environment.tenant_id == self.tenant.id
            && self.environment.application_id == self.application.id
            && self.environment.kind == EnvironmentKind::Production
    }

    /// Whether every level is ACTIVE (a suspended tenant disables the whole scope).
    pub fn active(&self) -> bool {
        self.tenant.status == TenantStatus::Active
            && self.application.status == ApplicationStatus::Active
            && self.environment.status == EnvironmentStatus::Active
    }

    /// The wire view (`OrganizationScope` of the internal contract). Ids, statuses and
    /// classification only.
    pub fn to_json(&self) -> Value {
        json!({
            "organizationRef": self.organization_ref,
            "capability": self.capability,
            "tenantId": self.tenant.id,
            "applicationId": self.application.id,
            "environmentId": self.environment.id,
            "tenant": {
                "id": self.tenant.id,
                "slug": self.tenant.slug,
                "classification": self.tenant.classification,
                "status": self.tenant.status,
                "revision": self.tenant.revision,
            },
            "application": {
                "id": self.application.id,
                "tenantId": self.application.tenant_id,
                "slug": self.application.slug,
                "status": self.application.status,
                "revision": self.application.revision,
            },
            "environment": {
                "id": self.environment.id,
                "tenantId": self.environment.tenant_id,
                "applicationId": self.environment.application_id,
                "kind": self.environment.kind,
                "status": self.environment.status,
                "revision": self.environment.revision,
            },
            "verified": self.verified(),
            "active": self.active(),
            "requestedBy": self.requested_by,
            "createdAt": self.created_at,
        })
    }
}

fn capability(value: &str) -> PlatformResult<&'static str> {
    ORGANIZATION_SCOPE_CAPABILITIES
        .iter()
        .find(|c| **c == value)
        .copied()
        .ok_or_else(|| PlatformError::Validation("unknown organization scope capability".into()))
}

struct ScopeRow {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    requested_by: String,
    created_at: DateTime<Utc>,
}

async fn scope_row<C: ConnectionTrait>(
    db: &C,
    organization_ref: Uuid,
    capability: &str,
) -> PlatformResult<Option<ScopeRow>> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT tenant_id, application_id, environment_id, requested_by, created_at \
             FROM platform_organization_scopes WHERE organization_ref = $1 AND capability = $2",
            [organization_ref.into(), capability.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => Some(ScopeRow {
            tenant_id: r.try_get("", "tenant_id")?,
            application_id: r.try_get("", "application_id")?,
            environment_id: r.try_get("", "environment_id")?,
            requested_by: r.try_get("", "requested_by")?,
            created_at: r.try_get("", "created_at")?,
        }),
        None => None,
    })
}

impl PlatformService {
    /// The organization's provisioned scope, `None` when it was never provisioned.
    pub async fn organization_scope(
        &self,
        organization_ref: Uuid,
        capability_name: &str,
    ) -> PlatformResult<Option<OrganizationScope>> {
        let cap = capability(capability_name)?;
        let Some(row) = scope_row(&self.db, organization_ref, cap).await? else {
            return Ok(None);
        };
        Ok(Some(OrganizationScope {
            organization_ref,
            capability: cap.to_string(),
            tenant: self.tenant(TenantId::from_uuid(row.tenant_id)).await?,
            application: self
                .application(ApplicationId::from_uuid(row.application_id))
                .await?,
            environment: self
                .environment(EnvironmentId::from_uuid(row.environment_id))
                .await?,
            requested_by: row.requested_by,
            created_at: row.created_at,
        }))
    }

    /// Creates (first call) or returns (every later call) the organization's FIRST_PARTY
    /// tenant + first-party application + PRODUCTION environment. `true` when this call
    /// created it. `requested_by` is the authenticated calling service.
    pub async fn provision_organization_scope(
        &self,
        op: &OperatorContext,
        requested_by: &str,
        organization_ref: Uuid,
        capability_name: &str,
    ) -> PlatformResult<(OrganizationScope, bool)> {
        let cap = capability(capability_name)?;
        if organization_ref.is_nil() {
            return Err(PlatformError::Validation(
                "the organization reference must be a non-nil UUID".into(),
            ));
        }
        if let Some(existing) = self.organization_scope(organization_ref, cap).await? {
            return Ok((existing, false));
        }
        let txn = self.db.begin().await?;
        // Concurrent provisions of the same organization serialize here; the loser sees
        // the winner's committed row below and returns it.
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [format!("platform-organization-scope:{organization_ref}:{cap}").into()],
        ))
        .await?;
        if scope_row(&txn, organization_ref, cap).await?.is_some() {
            txn.commit().await?;
            let existing = self
                .organization_scope(organization_ref, cap)
                .await?
                .ok_or(PlatformError::NotFound("organization scope"))?;
            return Ok((existing, false));
        }
        let now = self.clock.now();
        let slug = organization_tenant_slug(organization_ref);
        let tenant = self
            .create_tenant_in(
                &txn,
                NewTenant {
                    slug: slug.clone(),
                    display_name: format!("CF Meet organization {organization_ref}"),
                    classification: TenantClassification::FirstParty,
                    billing_currency: None,
                },
            )
            .await
            .map_err(|e| match e {
                // The reserved slug exists without a scope row: never adopt a tenant this
                // path did not create (it may be EXTERNAL or operator-owned).
                PlatformError::Conflict(_) => PlatformError::Conflict(
                    "the organization's reserved tenant slug is held by a tenant outside the organization scope registry"
                        .into(),
                ),
                other => other,
            })?;
        let application = self
            .create_application_in(
                &txn,
                tenant.id,
                NewApplication {
                    slug: ORGANIZATION_APPLICATION_SLUG.to_string(),
                    display_name: "CF Meet first-party organization application".to_string(),
                },
            )
            .await?;
        let environment = self
            .create_environment_in(&txn, application.id, EnvironmentKind::Production)
            .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO platform_organization_scopes (organization_ref, capability, tenant_id, \
               application_id, environment_id, requested_by, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            [
                organization_ref.into(),
                cap.into(),
                tenant.id.as_uuid().into(),
                application.id.as_uuid().into(),
                environment.id.as_uuid().into(),
                requested_by.into(),
                now.into(),
            ],
        ))
        .await?;
        let (t, a, e) = (
            tenant.id.as_uuid(),
            application.id.as_uuid(),
            environment.id.as_uuid(),
        );
        let reference = json!({ "organizationRef": organization_ref, "capability": cap });
        for entry in [
            AuditEntry::new(OperatorAction::TenantCreated, AuditTargetKind::Tenant, t)
                .scope(Some(t), None, None)
                .detail(
                    json!({ "classification": "FIRST_PARTY", "organizationRef": organization_ref }),
                ),
            AuditEntry::new(
                OperatorAction::ApplicationCreated,
                AuditTargetKind::Application,
                a,
            )
            .scope(Some(t), Some(a), None)
            .detail(reference.clone()),
            AuditEntry::new(
                OperatorAction::EnvironmentCreated,
                AuditTargetKind::Environment,
                e,
            )
            .scope(Some(t), Some(a), Some(e))
            .detail(json!({ "kind": "PRODUCTION", "organizationRef": organization_ref })),
            AuditEntry::new(
                OperatorAction::OrganizationScopeProvisioned,
                AuditTargetKind::Tenant,
                t,
            )
            .scope(Some(t), Some(a), Some(e))
            .detail(json!({
                "organizationRef": organization_ref,
                "capability": cap,
                "requestedBy": requested_by,
            })),
        ] {
            record(&txn, op, &entry, now).await?;
        }
        txn.commit().await?;
        tracing::info!(organization_ref = %organization_ref, tenant_id = %t, "organization scope provisioned");
        Ok((
            OrganizationScope {
                organization_ref,
                capability: cap.to_string(),
                tenant,
                application,
                environment,
                requested_by: requested_by.to_string(),
                created_at: now,
            },
            true,
        ))
    }

    /// Suspends or resumes the organization's tenant (idempotent: an unchanged status
    /// writes nothing). Unknown organization scope: `NotFound`.
    pub async fn set_organization_scope_status(
        &self,
        op: &OperatorContext,
        organization_ref: Uuid,
        capability_name: &str,
        status: TenantStatus,
    ) -> PlatformResult<OrganizationScope> {
        let current = self
            .organization_scope(organization_ref, capability_name)
            .await?
            .ok_or(PlatformError::NotFound("organization scope"))?;
        if current.tenant.status == status {
            return Ok(current);
        }
        let txn = self.db.begin().await?;
        let tenant = self
            .set_tenant_status_in(&txn, current.tenant.id, status)
            .await?;
        let t = tenant.id.as_uuid();
        record(
            &txn,
            op,
            &AuditEntry::new(OperatorAction::TenantStatusSet, AuditTargetKind::Tenant, t)
                .scope(Some(t), None, None)
                .detail(json!({
                    "status": status.as_str(),
                    "organizationRef": organization_ref,
                    "capability": current.capability,
                })),
            self.clock.now(),
        )
        .await?;
        txn.commit().await?;
        Ok(OrganizationScope { tenant, ..current })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_slugs_are_valid_tenant_slugs_and_capabilities_are_closed() {
        let id = Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap();
        let slug = organization_tenant_slug(id);
        assert_eq!(slug, "org-0123456789abcdef0123456789abcdef");
        assert!(crate::domain::Slug::parse(&slug).is_ok());
        assert!(crate::domain::Slug::parse(ORGANIZATION_APPLICATION_SLUG).is_ok());
        assert_eq!(capability("primary").unwrap(), "primary");
        for bad in ["", "PRIMARY", "calling", "primary "] {
            assert!(capability(bad).is_err(), "{bad}");
        }
    }
}
