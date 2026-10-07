//! Communication principals (participant identity), separate from API client identity.
//!
//! A credential proves which application / service account is calling. It does not prove
//! who takes part in a communication. Participants are [`ResolvedPrincipal`]s, produced
//! only by [`PrincipalResolver`] from an authenticated [`PlatformAuthContext`] plus an
//! opaque reference, and always resolved inside the context's tenant, application and
//! environment. Callers cannot widen that scope by passing identifiers: the resolver
//! takes none. A principal outside the scope is reported exactly like a principal that
//! does not exist.
//!
//! References are opaque. CF Meet stores no plaintext contact data here: E.164-style and
//! `tel:` / `sip:` values are refused, and TELEPHONE_ENDPOINT references must be opaque
//! UUID handles (to be issued by the future Contact Vault). SERVICE_ACCOUNT principals
//! are linked explicitly to an authoritative service account of the same environment.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde::Serialize;
use uuid::Uuid;

use super::{
    context::PlatformAuthContext, error::summarize_db_error, PlatformError, PlatformResult,
    PlatformService,
};
use crate::{
    domain::{
        ApplicationId, EnvironmentId, PrincipalId, PrincipalKind, PrincipalStatus,
        ServiceAccountId, ServiceAccountStatus, TenantId,
    },
    models::{principals, service_accounts},
};

pub const MAX_REFERENCE_LEN: usize = 255;

/// How an API consumer refers to a principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalReference {
    /// CF Meet principal id.
    Id(PrincipalId),
    /// Consumer-defined opaque reference within a principal category.
    External {
        kind: PrincipalKind,
        external_reference: String,
    },
}

/// Registration request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewPrincipal {
    /// Any category except SERVICE_ACCOUNT, identified by an opaque reference.
    External {
        kind: PrincipalKind,
        external_reference: String,
    },
    /// A principal explicitly linked to an authoritative service account.
    ServiceAccount(ServiceAccountId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrincipalView {
    pub id: PrincipalId,
    pub tenant_id: TenantId,
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub kind: PrincipalKind,
    pub external_reference: String,
    pub linked_service_account_id: Option<ServiceAccountId>,
    pub status: PrincipalStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<principals::Model> for PrincipalView {
    fn from(m: principals::Model) -> Self {
        Self {
            id: PrincipalId::from_uuid(m.id),
            tenant_id: TenantId::from_uuid(m.tenant_id),
            application_id: ApplicationId::from_uuid(m.application_id),
            environment_id: EnvironmentId::from_uuid(m.environment_id),
            kind: m.kind,
            external_reference: m.external_reference,
            linked_service_account_id: m.linked_service_account_id.map(ServiceAccountId::from_uuid),
            status: m.status,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// A communication principal resolved inside an authenticated context's scope.
/// Deliberately a different type from [`PlatformAuthContext`]; neither is derivable
/// from the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPrincipal {
    id: PrincipalId,
    kind: PrincipalKind,
    external_reference: String,
    linked_service_account_id: Option<ServiceAccountId>,
    tenant_id: TenantId,
    application_id: ApplicationId,
    environment_id: EnvironmentId,
}

impl ResolvedPrincipal {
    pub fn id(&self) -> PrincipalId {
        self.id
    }

    pub fn kind(&self) -> PrincipalKind {
        self.kind
    }

    pub fn external_reference(&self) -> &str {
        &self.external_reference
    }

    pub fn linked_service_account_id(&self) -> Option<ServiceAccountId> {
        self.linked_service_account_id
    }

    pub fn tenant_id(&self) -> TenantId {
        self.tenant_id
    }

    pub fn application_id(&self) -> ApplicationId {
        self.application_id
    }

    pub fn environment_id(&self) -> EnvironmentId {
        self.environment_id
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrincipalResolutionError {
    /// No such principal in the caller's scope — also returned for principals of other
    /// tenants / applications / environments (no existence oracle).
    #[error("principal not found")]
    NotFound,
    /// In scope, but suspended / deactivated (or its linked service account is not active).
    #[error("principal is not active")]
    Inactive,
    #[error("invalid principal reference")]
    InvalidReference,
    #[error("principal directory unavailable")]
    Unavailable { summary: String },
}

fn reference_error(message: &str) -> PlatformError {
    PlatformError::PolicyViolation(message.to_string())
}

fn looks_like_telephone_number(value: &str) -> bool {
    let bytes = value.as_bytes();
    let plus_digit = bytes
        .windows(2)
        .any(|w| w[0] == b'+' && w[1].is_ascii_digit());
    let lower = value.to_ascii_lowercase();
    plus_digit
        || ["tel:", "sip:", "sips:"]
            .iter()
            .any(|p| lower.starts_with(p))
}

/// Validates an opaque external reference for `kind` (mirrors the database `CHECK`s).
pub fn validate_external_reference(kind: PrincipalKind, value: &str) -> PlatformResult<String> {
    if value.is_empty()
        || value.len() > MAX_REFERENCE_LEN
        || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err(PlatformError::Validation(format!(
            "external reference must be 1-{MAX_REFERENCE_LEN} printable ASCII characters without spaces"
        )));
    }
    if looks_like_telephone_number(value) {
        return Err(reference_error(
            "plaintext telephone numbers / tel: / sip: URIs are not accepted as principal references; use an opaque reference",
        ));
    }
    match kind {
        PrincipalKind::ServiceAccount => Err(reference_error(
            "SERVICE_ACCOUNT principals are registered from a service account id, not from free text",
        )),
        PrincipalKind::TelephoneEndpoint => match Uuid::parse_str(value) {
            Ok(uuid) if uuid.hyphenated().to_string() == value => Ok(value.to_string()),
            _ => Err(reference_error(
                "TELEPHONE_ENDPOINT references must be opaque contact-reference UUIDs (lower-case, hyphenated)",
            )),
        },
        _ => Ok(value.to_string()),
    }
}

/// Trusted ownership scope for a registration (from a persisted environment).
struct OwnerScope {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
}

impl PlatformService {
    /// Registers (idempotently) a principal in `environment_id`. Returns the principal and
    /// whether it was newly created.
    pub async fn register_principal(
        &self,
        environment_id: EnvironmentId,
        new: NewPrincipal,
    ) -> PlatformResult<(PrincipalView, bool)> {
        let environment = self.environment(environment_id).await?;
        let scope = OwnerScope {
            tenant_id: environment.tenant_id.as_uuid(),
            application_id: environment.application_id.as_uuid(),
            environment_id: environment.id.as_uuid(),
        };
        let (kind, reference, linked) = match new {
            NewPrincipal::External {
                kind,
                external_reference,
            } => (
                kind,
                validate_external_reference(kind, &external_reference)?,
                None,
            ),
            NewPrincipal::ServiceAccount(service_account_id) => {
                let sa = service_accounts::Entity::find_by_id(service_account_id.as_uuid())
                    .one(&self.db)
                    .await?
                    .filter(|sa| {
                        sa.tenant_id == scope.tenant_id
                            && sa.application_id == scope.application_id
                            && sa.environment_id == scope.environment_id
                    })
                    .ok_or(PlatformError::NotFound("service account"))?;
                if sa.status == ServiceAccountStatus::Revoked {
                    return Err(PlatformError::InvalidState(
                        "a revoked service account cannot become a principal".to_string(),
                    ));
                }
                (
                    PrincipalKind::ServiceAccount,
                    sa.id.to_string(),
                    Some(sa.id),
                )
            }
        };

        let now = self.clock.now();
        let inserted = principals::Entity::insert(principals::ActiveModel {
            id: Set(PrincipalId::generate().as_uuid()),
            tenant_id: Set(scope.tenant_id),
            application_id: Set(scope.application_id),
            environment_id: Set(scope.environment_id),
            kind: Set(kind),
            external_reference: Set(reference.clone()),
            linked_service_account_id: Set(linked),
            status: Set(PrincipalStatus::Active),
            created_at: Set(now),
            updated_at: Set(now),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                principals::Column::TenantId,
                principals::Column::ApplicationId,
                principals::Column::EnvironmentId,
                principals::Column::Kind,
                principals::Column::ExternalReference,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await?;
        let model = principals::Entity::find()
            .filter(principals::Column::TenantId.eq(scope.tenant_id))
            .filter(principals::Column::ApplicationId.eq(scope.application_id))
            .filter(principals::Column::EnvironmentId.eq(scope.environment_id))
            .filter(principals::Column::Kind.eq(kind))
            .filter(principals::Column::ExternalReference.eq(reference))
            .one(&self.db)
            .await?
            .ok_or(PlatformError::NotFound("principal"))?;
        let created = inserted > 0;
        if created {
            tracing::info!(
                principal_id = %model.id,
                environment_id = %model.environment_id,
                kind = %model.kind,
                "principal registered"
            );
        }
        Ok((model.into(), created))
    }

    pub async fn principal(&self, id: PrincipalId) -> PlatformResult<PrincipalView> {
        principals::Entity::find_by_id(id.as_uuid())
            .one(&self.db)
            .await?
            .map(Into::into)
            .ok_or(PlatformError::NotFound("principal"))
    }

    pub async fn list_principals(
        &self,
        environment_id: EnvironmentId,
    ) -> PlatformResult<Vec<PrincipalView>> {
        Ok(principals::Entity::find()
            .filter(principals::Column::EnvironmentId.eq(environment_id.as_uuid()))
            .order_by_asc(principals::Column::Kind)
            .order_by_asc(principals::Column::ExternalReference)
            .all(&self.db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn set_principal_status(
        &self,
        id: PrincipalId,
        status: PrincipalStatus,
    ) -> PlatformResult<PrincipalView> {
        let model = principals::Entity::find_by_id(id.as_uuid())
            .one(&self.db)
            .await?
            .ok_or(PlatformError::NotFound("principal"))?;
        if model.status == status {
            return Ok(model.into());
        }
        let mut active: principals::ActiveModel = model.into();
        active.status = Set(status);
        active.updated_at = Set(self.clock.now());
        let model = active.update(&self.db).await?;
        tracing::info!(principal_id = %model.id, status = %model.status, "principal status changed");
        Ok(model.into())
    }
}

/// Resolves principals strictly inside an authenticated context's ownership scope.
#[derive(Clone)]
pub struct PrincipalResolver {
    db: DatabaseConnection,
}

impl PrincipalResolver {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    pub async fn resolve(
        &self,
        ctx: &PlatformAuthContext,
        reference: &PrincipalReference,
    ) -> Result<ResolvedPrincipal, PrincipalResolutionError> {
        let unavailable = |e: sea_orm::DbErr| PrincipalResolutionError::Unavailable {
            summary: summarize_db_error(&e),
        };
        let scoped = principals::Entity::find()
            .filter(principals::Column::TenantId.eq(ctx.tenant_id().as_uuid()))
            .filter(principals::Column::ApplicationId.eq(ctx.application_id().as_uuid()))
            .filter(principals::Column::EnvironmentId.eq(ctx.environment_id().as_uuid()));
        let query = match reference {
            PrincipalReference::Id(id) => scoped.filter(principals::Column::Id.eq(id.as_uuid())),
            PrincipalReference::External {
                kind,
                external_reference,
            } => {
                if *kind != PrincipalKind::ServiceAccount
                    && validate_external_reference(*kind, external_reference).is_err()
                {
                    return Err(PrincipalResolutionError::InvalidReference);
                }
                if external_reference.is_empty() || external_reference.len() > MAX_REFERENCE_LEN {
                    return Err(PrincipalResolutionError::InvalidReference);
                }
                scoped
                    .filter(principals::Column::Kind.eq(*kind))
                    .filter(principals::Column::ExternalReference.eq(external_reference.as_str()))
            }
        };
        let model = query
            .one(&self.db)
            .await
            .map_err(unavailable)?
            .ok_or(PrincipalResolutionError::NotFound)?;
        if model.status != PrincipalStatus::Active {
            return Err(PrincipalResolutionError::Inactive);
        }
        if let Some(sa_id) = model.linked_service_account_id {
            let active = service_accounts::Entity::find_by_id(sa_id)
                .filter(service_accounts::Column::EnvironmentId.eq(model.environment_id))
                .one(&self.db)
                .await
                .map_err(unavailable)?
                .is_some_and(|sa| sa.status == ServiceAccountStatus::Active);
            if !active {
                return Err(PrincipalResolutionError::Inactive);
            }
        }
        Ok(ResolvedPrincipal {
            id: PrincipalId::from_uuid(model.id),
            kind: model.kind,
            external_reference: model.external_reference,
            linked_service_account_id: model
                .linked_service_account_id
                .map(ServiceAccountId::from_uuid),
            tenant_id: TenantId::from_uuid(model.tenant_id),
            application_id: ApplicationId::from_uuid(model.application_id),
            environment_id: EnvironmentId::from_uuid(model.environment_id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telephone_numbers_are_refused_for_every_kind() {
        for kind in PrincipalKind::ALL {
            for phone in [
                "+2348012345678",
                "user+1",
                "tel:08012345678",
                "SIP:alice@example.org",
            ] {
                assert!(
                    validate_external_reference(*kind, phone).is_err(),
                    "{kind} accepted {phone}"
                );
            }
        }
    }

    #[test]
    fn telephone_endpoints_require_opaque_uuid_handles() {
        let handle = "6f1c2b9e-3a4d-4c5e-8f70-112233445566";
        assert!(validate_external_reference(PrincipalKind::TelephoneEndpoint, handle).is_ok());
        for bad in [
            "08012345678",
            "2348012345678",
            "6F1C2B9E-3A4D-4C5E-8F70-112233445566",
            "6f1c2b9e3a4d4c5e8f70112233445566",
            "contact-ref-1",
        ] {
            assert!(
                validate_external_reference(PrincipalKind::TelephoneEndpoint, bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn service_account_principals_cannot_come_from_free_text() {
        assert!(validate_external_reference(
            PrincipalKind::ServiceAccount,
            "6f1c2b9e-3a4d-4c5e-8f70-112233445566"
        )
        .is_err());
    }

    #[test]
    fn opaque_references_are_accepted_for_other_kinds() {
        for kind in [
            PrincipalKind::NativeCfUser,
            PrincipalKind::ExternalApplicationPrincipal,
            PrincipalKind::EnterpriseMember,
            PrincipalKind::TemporaryExternalParticipant,
            PrincipalKind::BotOrService,
        ] {
            for ok in ["user-123", "12345", "acct:9f8e", "u_ABC.def"] {
                assert!(validate_external_reference(kind, ok).is_ok(), "{kind} {ok}");
            }
            for bad in ["", "has space", &"x".repeat(256), "naïve"] {
                assert!(
                    validate_external_reference(kind, bad).is_err(),
                    "{kind} {bad}"
                );
            }
        }
    }
}
