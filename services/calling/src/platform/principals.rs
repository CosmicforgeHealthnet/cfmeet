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
    ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder,
};
use serde::Serialize;
use uuid::Uuid;

use std::sync::Arc;

use super::{
    context::PlatformAuthContext, error::summarize_db_error, projection::IdentityProjection,
    PlatformError, PlatformResult, PlatformService,
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
/// Reserved prefix of the reference a tombstoned principal carries (Part H §80.42,
/// `retention::tombstone`): `tombstoned:<principal id>`.
pub const TOMBSTONE_REFERENCE_PREFIX: &str = "tombstoned:";

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
    // Reserved for tombstoned principals (Part H §80.42): a registration can never take
    // (or pre-empt) a tombstone reference.
    if value.starts_with(TOMBSTONE_REFERENCE_PREFIX) {
        return Err(reference_error(
            "the `tombstoned:` prefix is reserved and is not accepted as a principal reference",
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

impl PlatformService {
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
}

/// Resolves principals strictly inside an authenticated context's ownership scope.
///
/// Principals are registered in Platform Access (ADR 0032); Calling resolves them from
/// its identity projection — the rows its foreign keys reference — and, when a
/// reference is not projected yet (registered a moment ago), catches the projection up
/// from the change feed once before answering NOT_FOUND.
#[derive(Clone)]
pub struct PrincipalResolver {
    db: DatabaseConnection,
    projection: Option<Arc<IdentityProjection>>,
}

impl PrincipalResolver {
    /// A resolver over the projection only (no catch-up, no registration).
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            db,
            projection: None,
        }
    }

    /// A resolver that catches the projection up and registers public parties through
    /// Platform Access.
    pub fn with_projection(db: DatabaseConnection, projection: Arc<IdentityProjection>) -> Self {
        Self {
            db,
            projection: Some(projection),
        }
    }

    /// The TELEPHONE_ENDPOINT principal of a protected public party (Part E §45): its
    /// only reference is the opaque ContactReference Privacy Relay issued — never a
    /// number. Registered on first use in the scope (by Platform Access, in the signed
    /// delegated scope of `service_account_id`), then resolved like any principal.
    pub async fn ensure_public_party_principal(
        &self,
        tenant_id: TenantId,
        application_id: ApplicationId,
        environment_id: EnvironmentId,
        service_account_id: ServiceAccountId,
        contact_reference: Uuid,
    ) -> Result<ResolvedPrincipal, PrincipalResolutionError> {
        let reference = PrincipalReference::External {
            kind: PrincipalKind::TelephoneEndpoint,
            external_reference: contact_reference.hyphenated().to_string(),
        };
        match self
            .resolve_local(tenant_id, application_id, environment_id, &reference)
            .await
        {
            Err(PrincipalResolutionError::NotFound) => {}
            other => return other,
        }
        let projection = self.projection.as_ref().ok_or(PrincipalResolutionError::Unavailable {
            summary: "platform access is not configured".to_string(),
        })?;
        let unavailable = |e: &dyn std::fmt::Display| PrincipalResolutionError::Unavailable {
            summary: e.to_string(),
        };
        let kind: String = self
            .db
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT kind FROM platform_environments WHERE id = $1 AND application_id = $2 AND tenant_id = $3",
                [
                    environment_id.as_uuid().into(),
                    application_id.as_uuid().into(),
                    tenant_id.as_uuid().into(),
                ],
            ))
            .await
            .map_err(|e| unavailable(&summarize_db_error(&e)))?
            .ok_or(PrincipalResolutionError::NotFound)?
            .try_get_by_index(0)
            .map_err(|e| unavailable(&summarize_db_error(&e)))?;
        let kind = match kind.as_str() {
            "PRODUCTION" => cf_service_auth::ApplicationEnvironmentKind::Production,
            "TEST" => cf_service_auth::ApplicationEnvironmentKind::Test,
            _ => cf_service_auth::ApplicationEnvironmentKind::Sandbox,
        };
        let scope = cf_service_auth::DelegatedPlatformContext::new(
            tenant_id.as_uuid(),
            application_id.as_uuid(),
            environment_id.as_uuid(),
            kind,
            service_account_id.as_uuid(),
        );
        let snapshot = projection
            .authority()
            .ensure_telephone_endpoint(&scope, contact_reference)
            .await
            .map_err(|e| match e {
                super::access::AuthorityError::Refused(code) if code == "INVALID_STATE" => {
                    PrincipalResolutionError::Inactive
                }
                other => unavailable(&other),
            })?;
        projection
            .apply_principal(&snapshot)
            .await
            .map_err(|e| unavailable(&e))?;
        self.resolve_local(tenant_id, application_id, environment_id, &reference)
            .await
    }

    pub async fn resolve(
        &self,
        ctx: &PlatformAuthContext,
        reference: &PrincipalReference,
    ) -> Result<ResolvedPrincipal, PrincipalResolutionError> {
        self.resolve_in(
            ctx.tenant_id(),
            ctx.application_id(),
            ctx.environment_id(),
            reference,
        )
        .await
    }

    /// Resolves inside an ownership scope that was itself established by authentication
    /// (an API credential's context or a verified signaling credential) — never by
    /// request data. A principal not projected yet triggers one projection catch-up.
    pub async fn resolve_in(
        &self,
        tenant_id: TenantId,
        application_id: ApplicationId,
        environment_id: EnvironmentId,
        reference: &PrincipalReference,
    ) -> Result<ResolvedPrincipal, PrincipalResolutionError> {
        match self
            .resolve_local(tenant_id, application_id, environment_id, reference)
            .await
        {
            Err(PrincipalResolutionError::NotFound) => {
                let Some(projection) = &self.projection else {
                    return Err(PrincipalResolutionError::NotFound);
                };
                projection
                    .catch_up()
                    .await
                    .map_err(|e| PrincipalResolutionError::Unavailable {
                        summary: e.to_string(),
                    })?;
                self.resolve_local(tenant_id, application_id, environment_id, reference)
                    .await
            }
            other => other,
        }
    }

    async fn resolve_local(
        &self,
        tenant_id: TenantId,
        application_id: ApplicationId,
        environment_id: EnvironmentId,
        reference: &PrincipalReference,
    ) -> Result<ResolvedPrincipal, PrincipalResolutionError> {
        let unavailable = |e: sea_orm::DbErr| PrincipalResolutionError::Unavailable {
            summary: summarize_db_error(&e),
        };
        let scoped = principals::Entity::find()
            .filter(principals::Column::TenantId.eq(tenant_id.as_uuid()))
            .filter(principals::Column::ApplicationId.eq(application_id.as_uuid()))
            .filter(principals::Column::EnvironmentId.eq(environment_id.as_uuid()));
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
