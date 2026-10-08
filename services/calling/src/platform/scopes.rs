//! Server-authoritative scopes.
//!
//! * The [`ScopeRegistry`] is the only source of valid scope names. Scope values stay
//!   strings in the database so new scopes need no destructive schema change, but a
//!   string that is not in the registry is never granted and never honoured.
//! * An application holds a set of *allowed* scopes; a credential's *granted* scopes must
//!   be a subset (validated here and enforced by a foreign key).
//! * Trusted scopes are declared explicitly ([`ScopeTrust::FirstPartyOnly`]) and gated on
//!   the tenant's persisted trust classification, never on an application or product name.
//!
//! Requests can never add scopes: the authenticated context carries exactly the granted
//! scopes loaded from the database.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
};
use serde::Serialize;

use super::{PlatformError, PlatformResult, PlatformService};
use crate::{
    domain::{ApplicationId, TenantClassification},
    models::application_scopes,
};

/// Well-known neutral CF Meet scope names.
pub mod names {
    /// Resolve communication principals inside the credential's application environment.
    pub const PRINCIPALS_READ: &str = "principals.read";
    /// Register / update communication principals inside the credential's environment.
    pub const PRINCIPALS_WRITE: &str = "principals.write";
    /// Read calls (reserved for the Calling API).
    pub const CALLS_READ: &str = "calls.read";
    /// Create and control calls (reserved for the Calling API).
    pub const CALLS_WRITE: &str = "calls.write";
    /// Private-network, low-latency event delivery (reserved; trusted).
    pub const EVENTS_PRIVATE_DELIVERY: &str = "events.private_delivery";
    /// Read Privacy Relay communication contexts of the credential's environment.
    pub const PRIVACY_CONTEXTS_READ: &str = "privacy.contexts.read";
    /// Create, update and change the lifecycle of communication contexts.
    pub const PRIVACY_CONTEXTS_WRITE: &str = "privacy.contexts.write";
    /// Read Privacy Relay sessions of the credential's environment.
    pub const PRIVACY_SESSIONS_READ: &str = "privacy.sessions.read";
    /// Create Privacy Relay sessions and change their lifecycle.
    pub const PRIVACY_SESSIONS_WRITE: &str = "privacy.sessions.write";
    /// Read protected contacts (masked) through the Contact Vault (Part D §31).
    pub const PRIVACY_CONTACTS_READ: &str = "privacy.contacts.read";
    /// Create / replace / disable protected contacts through the Contact Vault.
    pub const PRIVACY_CONTACTS_WRITE: &str = "privacy.contacts.write";
    /// Assert contact verification state (a trusted integration; §33).
    pub const PRIVACY_CONTACTS_VERIFY: &str = "privacy.contacts.verify";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeTrust {
    /// Grantable to any tenant's applications.
    Standard,
    /// Grantable only to applications of FIRST_PARTY tenants.
    FirstPartyOnly,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub trust: ScopeTrust,
}

const STANDARD_SCOPES: &[ScopeDefinition] = &[
    ScopeDefinition {
        name: names::PRINCIPALS_READ,
        description: "Resolve communication principals within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRINCIPALS_WRITE,
        description: "Register and update communication principals within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::CALLS_READ,
        description: "Read calls (reserved for the Calling API)",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::CALLS_WRITE,
        description: "Create and control calls (reserved for the Calling API)",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_CONTEXTS_READ,
        description: "Read Privacy Relay communication contexts within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_CONTEXTS_WRITE,
        description: "Create, update and change the lifecycle of Privacy Relay communication contexts within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_SESSIONS_READ,
        description: "Read Privacy Relay sessions within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_SESSIONS_WRITE,
        description: "Create Privacy Relay sessions and change their lifecycle within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_CONTACTS_READ,
        description: "Read protected contacts (masked only) through the Privacy Relay Contact Vault",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_CONTACTS_WRITE,
        description: "Store, replace and disable protected telephone contacts through the Privacy Relay Contact Vault",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::PRIVACY_CONTACTS_VERIFY,
        description: "Assert the verification state of protected contacts (trusted verification integrations only)",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::EVENTS_PRIVATE_DELIVERY,
        description: "Private-network low-latency event delivery (reserved; trusted)",
        trust: ScopeTrust::FirstPartyOnly,
    },
];

/// A scope name that exists in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Scope(String);

impl Scope {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Set of scopes held by an authenticated credential.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ScopeSet(BTreeSet<Scope>);

impl ScopeSet {
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|s| s.as_str() == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.0.iter().map(|s| s.0.clone()).collect()
    }
}

#[derive(Debug, Clone)]
pub struct ScopeRegistry {
    definitions: BTreeMap<&'static str, ScopeDefinition>,
}

impl Default for ScopeRegistry {
    fn default() -> Self {
        Self::standard()
    }
}

impl ScopeRegistry {
    /// The CF Meet scope registry.
    pub fn standard() -> Self {
        Self {
            definitions: STANDARD_SCOPES
                .iter()
                .map(|d| (d.name, d.clone()))
                .collect(),
        }
    }

    pub fn definitions(&self) -> impl Iterator<Item = &ScopeDefinition> {
        self.definitions.values()
    }

    pub fn definition(&self, name: &str) -> Option<&ScopeDefinition> {
        self.definitions.get(name)
    }

    /// Validates that `name` exists and may be held by an application of a tenant with
    /// `classification`.
    pub fn grantable(
        &self,
        name: &str,
        classification: TenantClassification,
    ) -> PlatformResult<Scope> {
        let definition = self.definition(name).ok_or_else(|| {
            PlatformError::Validation(format!(
                "unknown scope `{}`",
                name.chars().take(100).collect::<String>()
            ))
        })?;
        match (definition.trust, classification) {
            (ScopeTrust::FirstPartyOnly, TenantClassification::External) => {
                Err(PlatformError::PolicyViolation(format!(
                    "scope `{}` may only be granted to FIRST_PARTY tenants",
                    definition.name
                )))
            }
            _ => Ok(Scope(definition.name.to_string())),
        }
    }

    /// Builds the authenticated scope set from persisted grants. Unknown strings and
    /// grants not permitted for the tenant classification are dropped (fail closed).
    pub(crate) fn effective(
        &self,
        persisted: impl IntoIterator<Item = String>,
        classification: TenantClassification,
    ) -> ScopeSet {
        ScopeSet(
            persisted
                .into_iter()
                .filter_map(|name| self.grantable(&name, classification).ok())
                .collect(),
        )
    }

    /// Validates a requested credential scope list: registry membership, trust
    /// classification and de-duplication. Subset-of-application is checked by the caller.
    pub(crate) fn validate_requested(
        &self,
        requested: &[String],
        classification: TenantClassification,
    ) -> PlatformResult<BTreeSet<Scope>> {
        requested
            .iter()
            .map(|name| self.grantable(name.trim(), classification))
            .collect()
    }
}

/// Application allowed-scope row as shown to operators.
#[derive(Debug, Clone, Serialize)]
pub struct ApplicationScopeView {
    pub application_id: ApplicationId,
    pub scope: String,
    pub granted_at: DateTime<Utc>,
}

pub(crate) async fn allowed_scopes<C: ConnectionTrait>(
    db: &C,
    application_id: ApplicationId,
) -> PlatformResult<BTreeSet<String>> {
    Ok(application_scopes::Entity::find()
        .filter(application_scopes::Column::ApplicationId.eq(application_id.as_uuid()))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.scope)
        .collect())
}

impl PlatformService {
    /// Adds `scope` to the application's allowed set. Idempotent.
    pub async fn grant_application_scope(
        &self,
        application_id: ApplicationId,
        scope: &str,
    ) -> PlatformResult<ApplicationScopeView> {
        let application = self.application(application_id).await?;
        let tenant = self.tenant(application.tenant_id).await?;
        let scope = self.registry.grantable(scope, tenant.classification)?;
        let now = self.clock.now();
        application_scopes::Entity::insert(application_scopes::ActiveModel {
            application_id: Set(application_id.as_uuid()),
            scope: Set(scope.as_str().to_string()),
            tenant_id: Set(application.tenant_id.as_uuid()),
            granted_at: Set(now),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                application_scopes::Column::ApplicationId,
                application_scopes::Column::Scope,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(&self.db)
        .await?;
        let row = application_scopes::Entity::find_by_id((
            application_id.as_uuid(),
            scope.as_str().to_string(),
        ))
        .one(&self.db)
        .await?
        .ok_or(PlatformError::NotFound("application scope"))?;
        tracing::info!(
            application_id = %application_id,
            scope = scope.as_str(),
            "application scope allowed"
        );
        Ok(ApplicationScopeView {
            application_id,
            scope: row.scope,
            granted_at: row.granted_at,
        })
    }

    /// Removes `scope` from the application's allowed set and, in the same statement
    /// (foreign-key cascade), from every credential of the application. Idempotent.
    /// Returns whether the scope was allowed before.
    pub async fn revoke_application_scope(
        &self,
        application_id: ApplicationId,
        scope: &str,
    ) -> PlatformResult<bool> {
        self.application(application_id).await?;
        let result = application_scopes::Entity::delete_many()
            .filter(application_scopes::Column::ApplicationId.eq(application_id.as_uuid()))
            .filter(application_scopes::Column::Scope.eq(scope))
            .exec(&self.db)
            .await?;
        tracing::info!(
            application_id = %application_id,
            scope = scope.chars().take(100).collect::<String>(),
            removed = result.rows_affected > 0,
            "application scope withdrawn"
        );
        Ok(result.rows_affected > 0)
    }

    pub async fn list_application_scopes(
        &self,
        application_id: ApplicationId,
    ) -> PlatformResult<Vec<ApplicationScopeView>> {
        self.application(application_id).await?;
        Ok(application_scopes::Entity::find()
            .filter(application_scopes::Column::ApplicationId.eq(application_id.as_uuid()))
            .order_by_asc(application_scopes::Column::Scope)
            .all(&self.db)
            .await?
            .into_iter()
            .map(|row| ApplicationScopeView {
                application_id,
                scope: row.scope,
                granted_at: row.granted_at,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_scopes_are_neutral_and_well_formed() {
        let registry = ScopeRegistry::standard();
        for def in registry.definitions() {
            assert!(def.name.split('.').all(|part| !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
            assert!(def.name.contains('.'));
        }
    }

    #[test]
    fn unknown_scopes_are_rejected() {
        let registry = ScopeRegistry::standard();
        assert!(matches!(
            registry.grantable("admin.everything", TenantClassification::FirstParty),
            Err(PlatformError::Validation(_))
        ));
    }

    #[test]
    fn trusted_scopes_follow_classification_not_names() {
        let registry = ScopeRegistry::standard();
        assert!(registry
            .grantable(
                names::EVENTS_PRIVATE_DELIVERY,
                TenantClassification::FirstParty
            )
            .is_ok());
        assert!(matches!(
            registry.grantable(
                names::EVENTS_PRIVATE_DELIVERY,
                TenantClassification::External
            ),
            Err(PlatformError::PolicyViolation(_))
        ));
        for classification in TenantClassification::ALL {
            assert!(registry
                .grantable(names::PRINCIPALS_READ, *classification)
                .is_ok());
        }
    }

    #[test]
    fn effective_scopes_drop_unknown_and_untrusted_grants() {
        let registry = ScopeRegistry::standard();
        let set = registry.effective(
            vec![
                names::PRINCIPALS_READ.to_string(),
                "legacy.removed".to_string(),
                names::EVENTS_PRIVATE_DELIVERY.to_string(),
            ],
            TenantClassification::External,
        );
        assert_eq!(set.names(), vec![names::PRINCIPALS_READ.to_string()]);
    }
}
