//! Server-authoritative scopes.
//!
//! * The [`ScopeRegistry`] is the only source of valid scope names. Scope values stay
//!   strings in the database so new scopes need no destructive schema change, but a
//!   string that is not in the registry is never granted and never honoured.
//! * An application holds a set of *allowed* scopes; a credential's *granted* scopes must
//!   be a subset (validated here and enforced by a foreign key).
//! * Trusted scopes are declared explicitly ([`ScopeTrust::FirstPartyOnly`]) and gated on
//!   the tenant's persisted trust classification, never on an application or product name.
//! * Privileged operator scopes ([`ScopeTrust::PrivilegedOperator`], `numbers.admin`) are
//!   never granted through the ordinary path: only an explicit, audited operator grant
//!   (grant basis `PRIVILEGED_OPERATOR`) to an application of a FIRST_PARTY tenant, and
//!   they are honoured only with that basis.
//! * An application's allowed scope may be restricted to environment kinds; a credential
//!   of another environment can neither be granted nor exercise it.
//!
//! Restriction levels: tenant (trust classification), application (allowed set),
//! environment (allowed environment kinds), service account (the credential's granted
//! subset — every credential belongs to one service account) and authorized principal
//! (`restrictions`: a credential limited to listed principals).
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
    domain::{ApplicationId, EnvironmentKind, TenantClassification},
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
    /// Read privacy-safe call evidence of Privacy Relay sessions (Part D §36).
    pub const PRIVACY_EVIDENCE_READ: &str = "privacy.evidence.read";
    /// Read enterprise calling configuration (Part F §19).
    pub const ENTERPRISE_CONFIG_READ: &str = "enterprise.config.read";
    /// Create / change / disable enterprise calling configuration.
    pub const ENTERPRISE_CONFIG_WRITE: &str = "enterprise.config.write";
    /// Place and control calls to enterprise destinations (extensions).
    pub const ENTERPRISE_CALLS_USE: &str = "enterprise.calls.use";
    /// Read voicemail messages of mailboxes the request is authorized for.
    pub const ENTERPRISE_VOICEMAIL_READ: &str = "enterprise.voicemail.read";
    /// Change read state, delete voicemail; configure mailboxes.
    pub const ENTERPRISE_VOICEMAIL_MANAGE: &str = "enterprise.voicemail.manage";
    /// Configure call-recording policies and start / stop recordings.
    pub const ENTERPRISE_RECORDING_MANAGE: &str = "enterprise.recording.manage";
    /// Read call-recording metadata and request authorized media access (audited).
    pub const ENTERPRISE_RECORDING_READ: &str = "enterprise.recording.read";
    /// Read customer webhook subscriptions and their delivery history (Part G §68).
    pub const WEBHOOKS_READ: &str = "webhooks.read";
    /// Create, change, delete customer webhook subscriptions, rotate their signing secret
    /// and replay deliveries (Part G §68).
    pub const WEBHOOKS_MANAGE: &str = "webhooks.manage";
    /// Read the application's usage facts and effective limits (Part G §69 / §70).
    pub const USAGE_READ: &str = "usage.read";
    /// Numbering (Part IV): search available public numbers.
    pub const NUMBERS_SEARCH: &str = "numbers.search";
    /// Numbering: read the application's Number Resources and DCN assignments.
    pub const NUMBERS_READ: &str = "numbers.read";
    /// Numbering: reserve, provision, configure and release numbers.
    pub const NUMBERS_MANAGE: &str = "numbers.manage";
    /// Numbering: assign numbers to principals / organizations and change routing.
    pub const NUMBERS_ASSIGN: &str = "numbers.assign";
    /// Numbering: read KYC / eligibility references and decisions.
    pub const NUMBERS_KYC_READ: &str = "numbers.kyc.read";
    /// Numbering: submit and update KYC / eligibility material.
    pub const NUMBERS_KYC_MANAGE: &str = "numbers.kyc.manage";
    /// Numbering: port numbers in / out.
    pub const NUMBERS_PORTABILITY: &str = "numbers.portability";
    /// Numbering: privileged internal operator authority (quarantine, forced release,
    /// inventory administration). Never an ordinary customer grant.
    pub const NUMBERS_ADMIN: &str = "numbers.admin";
    /// SMS: send messages from numbers the application is authorized for.
    pub const SMS_SEND: &str = "sms.send";
    /// SMS: read messages and delivery state of the application.
    pub const SMS_READ: &str = "sms.read";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeTrust {
    /// Grantable to any tenant's applications.
    Standard,
    /// Grantable only to applications of FIRST_PARTY tenants.
    FirstPartyOnly,
    /// Privileged internal operator authority: only an explicit operator grant
    /// (`PRIVILEGED_OPERATOR` basis) to a FIRST_PARTY tenant's application.
    PrivilegedOperator,
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
        name: names::PRIVACY_EVIDENCE_READ,
        description: "Read privacy-safe call evidence (timing, outcome, duration; never a telephone number) of Privacy Relay sessions within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_CONFIG_READ,
        description: "Read enterprise calling configuration (policies, departments, extensions, ring groups, queues, IVR, business hours, forwarding, mailboxes, recording policies) and its audit within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_CONFIG_WRITE,
        description: "Create, change and disable enterprise calling configuration within the credential's application environment (never implies calls.* or voicemail / recording access)",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_CALLS_USE,
        description: "Place calls to enterprise destinations (extensions) and send IVR input within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_VOICEMAIL_READ,
        description: "List voicemail messages and request short-lived authorized media access for mailboxes owned by the named principal",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_VOICEMAIL_MANAGE,
        description: "Mark voicemail read / unread, delete voicemail and configure voicemail mailboxes",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_RECORDING_MANAGE,
        description: "Configure call-recording policies, record consent evidence and start / stop call recordings",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::ENTERPRISE_RECORDING_READ,
        description: "Read call-recording metadata and request short-lived, audited media access",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::WEBHOOKS_READ,
        description: "Read the application's customer webhook subscriptions and delivery history",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::WEBHOOKS_MANAGE,
        description: "Create, change and delete customer webhook subscriptions, rotate signing secrets and replay deliveries",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::USAGE_READ,
        description: "Read the application's usage facts (consumption, not billing) and effective limits",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_SEARCH,
        description: "Search available public numbers (inventory observations are not guaranteed allocations)",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_READ,
        description: "Read the application's Number Resources, capabilities and DCN assignments within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_MANAGE,
        description: "Reserve, provision, configure and release Number Resources within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_ASSIGN,
        description: "Assign Number Resources to principals or organizations and change their routing within the credential's application environment",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_KYC_READ,
        description: "Read KYC / eligibility references and decisions of the application's number requests",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_KYC_MANAGE,
        description: "Submit and update KYC / eligibility material for the application's number requests",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_PORTABILITY,
        description: "Request and manage number porting for the application",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::NUMBERS_ADMIN,
        description: "Privileged internal operator authority over Numbering (quarantine, forced release, inventory administration)",
        trust: ScopeTrust::PrivilegedOperator,
    },
    ScopeDefinition {
        name: names::SMS_SEND,
        description: "Send SMS from numbers the application is authorized to use",
        trust: ScopeTrust::Standard,
    },
    ScopeDefinition {
        name: names::SMS_READ,
        description: "Read the application's SMS messages and delivery state",
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
            (ScopeTrust::PrivilegedOperator, _) => Err(PlatformError::PolicyViolation(format!(
                "scope `{}` is privileged operator authority and requires an explicit privileged operator grant",
                definition.name
            ))),
            _ => Ok(Scope(definition.name.to_string())),
        }
    }

    /// Validates a PRIVILEGED operator grant: the scope must be a privileged operator
    /// scope and the tenant FIRST_PARTY.
    pub fn grantable_privileged(
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
        if definition.trust != ScopeTrust::PrivilegedOperator {
            return Err(PlatformError::PolicyViolation(format!(
                "scope `{}` is not a privileged operator scope; grant it normally",
                definition.name
            )));
        }
        if classification != TenantClassification::FirstParty {
            return Err(PlatformError::PolicyViolation(format!(
                "privileged operator scope `{}` may only be granted to FIRST_PARTY tenants",
                definition.name
            )));
        }
        Ok(Scope(definition.name.to_string()))
    }

    /// Whether a persisted grant is honoured: registry scope, trust classification and
    /// (for privileged scopes) the PRIVILEGED_OPERATOR basis, and the credential's
    /// environment kind inside the grant's environment restriction.
    pub fn honours(&self, grant: &PersistedGrant, classification: TenantClassification) -> bool {
        let Some(definition) = self.definition(&grant.scope) else {
            return false;
        };
        let trust_ok = match definition.trust {
            ScopeTrust::Standard => true,
            ScopeTrust::FirstPartyOnly => classification == TenantClassification::FirstParty,
            ScopeTrust::PrivilegedOperator => {
                classification == TenantClassification::FirstParty
                    && grant.grant_basis == GrantBasis::PrivilegedOperator
            }
        };
        trust_ok && grant.environment_allowed
    }

    /// Builds the authenticated scope set from persisted grants. Unknown strings, grants
    /// not permitted for the tenant classification, privileged scopes without their
    /// basis and grants restricted to other environment kinds are dropped (fail closed).
    pub fn effective(
        &self,
        persisted: impl IntoIterator<Item = PersistedGrant>,
        classification: TenantClassification,
    ) -> ScopeSet {
        ScopeSet(
            persisted
                .into_iter()
                .filter(|grant| self.honours(grant, classification))
                .map(|grant| Scope(grant.scope))
                .collect(),
        )
    }

    /// Validates a requested credential scope list: registry membership, trust
    /// classification and de-duplication. A privileged scope passes here only for a
    /// FIRST_PARTY tenant; that the APPLICATION holds it with the PRIVILEGED_OPERATOR
    /// basis (and every other subset rule) is checked by the caller.
    pub(crate) fn validate_requested(
        &self,
        requested: &[String],
        classification: TenantClassification,
    ) -> PlatformResult<BTreeSet<Scope>> {
        requested
            .iter()
            .map(|name| {
                let name = name.trim();
                match self.definition(name).map(|d| d.trust) {
                    Some(ScopeTrust::PrivilegedOperator) => {
                        self.grantable_privileged(name, classification)
                    }
                    _ => self.grantable(name, classification),
                }
            })
            .collect()
    }

    /// Whether `name` is a privileged operator scope.
    pub fn is_privileged(&self, name: &str) -> bool {
        self.definition(name)
            .is_some_and(|d| d.trust == ScopeTrust::PrivilegedOperator)
    }
}

text_enum!(
    /// Why an application holds a scope.
    GrantBasis {
        Standard = "STANDARD",
        PrivilegedOperator = "PRIVILEGED_OPERATOR",
    }
);

/// A credential's persisted grant, joined with its application grant, as evaluated at
/// authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedGrant {
    pub scope: String,
    pub grant_basis: GrantBasis,
    /// Whether the credential's environment kind is inside the application grant's
    /// environment restriction.
    pub environment_allowed: bool,
}

/// Application allowed-scope row as shown to operators.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationScopeView {
    pub application_id: ApplicationId,
    pub scope: String,
    /// `None` = every environment kind.
    pub allowed_environment_kinds: Option<Vec<EnvironmentKind>>,
    pub grant_basis: GrantBasis,
    pub granted_at: DateTime<Utc>,
}

/// An application's allowed scope with its restriction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedScope {
    pub environment_kinds: Option<Vec<EnvironmentKind>>,
    pub grant_basis: GrantBasis,
}

impl AllowedScope {
    pub fn allows(&self, kind: EnvironmentKind) -> bool {
        self.environment_kinds
            .as_ref()
            .is_none_or(|kinds| kinds.contains(&kind))
    }
}

fn parse_kinds(raw: Option<Vec<String>>) -> Option<Vec<EnvironmentKind>> {
    // Unknown persisted values are dropped; the CHECK constraint keeps them out anyway.
    raw.map(|kinds| {
        kinds
            .iter()
            .filter_map(|k| EnvironmentKind::parse(k))
            .collect()
    })
}

fn view(row: application_scopes::Model) -> ApplicationScopeView {
    ApplicationScopeView {
        application_id: ApplicationId::from_uuid(row.application_id),
        allowed_environment_kinds: parse_kinds(row.allowed_environment_kinds),
        grant_basis: GrantBasis::parse(&row.grant_basis).unwrap_or(GrantBasis::Standard),
        scope: row.scope,
        granted_at: row.granted_at,
    }
}

pub(crate) async fn allowed_scopes<C: ConnectionTrait>(
    db: &C,
    application_id: ApplicationId,
) -> PlatformResult<BTreeMap<String, AllowedScope>> {
    Ok(application_scopes::Entity::find()
        .filter(application_scopes::Column::ApplicationId.eq(application_id.as_uuid()))
        .all(db)
        .await?
        .into_iter()
        .map(|row| {
            let v = view(row);
            (
                v.scope,
                AllowedScope {
                    environment_kinds: v.allowed_environment_kinds,
                    grant_basis: v.grant_basis,
                },
            )
        })
        .collect())
}

fn normalized_kinds(kinds: Option<&[EnvironmentKind]>) -> PlatformResult<Option<Vec<String>>> {
    match kinds {
        None => Ok(None),
        Some([]) => Err(PlatformError::Validation(
            "an environment restriction must name at least one environment kind".to_string(),
        )),
        Some(kinds) => {
            let set: BTreeSet<_> = kinds.iter().map(|k| k.as_str().to_string()).collect();
            Ok(Some(set.into_iter().collect()))
        }
    }
}

impl PlatformService {
    /// Adds `scope` to the application's allowed set (every environment kind).
    /// Idempotent; re-granting an environment-restricted scope lifts the restriction.
    pub async fn grant_application_scope(
        &self,
        application_id: ApplicationId,
        scope: &str,
    ) -> PlatformResult<ApplicationScopeView> {
        self.grant_application_scope_in(&self.db, application_id, scope, None)
            .await
    }

    /// Adds `scope` restricted to `environment_kinds` (`None` = every kind).
    pub async fn grant_application_scope_restricted(
        &self,
        application_id: ApplicationId,
        scope: &str,
        environment_kinds: Option<&[EnvironmentKind]>,
    ) -> PlatformResult<ApplicationScopeView> {
        self.grant_application_scope_in(&self.db, application_id, scope, environment_kinds)
            .await
    }

    pub(crate) async fn grant_application_scope_in<C: ConnectionTrait>(
        &self,
        db: &C,
        application_id: ApplicationId,
        scope: &str,
        environment_kinds: Option<&[EnvironmentKind]>,
    ) -> PlatformResult<ApplicationScopeView> {
        let application = self.application(application_id).await?;
        let tenant = self.tenant(application.tenant_id).await?;
        let scope = self.registry.grantable(scope, tenant.classification)?;
        self.upsert_grant(
            db,
            &application,
            &scope,
            normalized_kinds(environment_kinds)?,
            GrantBasis::Standard,
        )
        .await
    }

    /// Grants a PRIVILEGED operator scope (`numbers.admin`) to an application of a
    /// FIRST_PARTY tenant. Only reachable through the audited operator path.
    pub(crate) async fn grant_privileged_scope_in<C: ConnectionTrait>(
        &self,
        db: &C,
        application_id: ApplicationId,
        scope: &str,
        environment_kinds: Option<&[EnvironmentKind]>,
    ) -> PlatformResult<ApplicationScopeView> {
        let application = self.application(application_id).await?;
        let tenant = self.tenant(application.tenant_id).await?;
        let scope = self
            .registry
            .grantable_privileged(scope, tenant.classification)?;
        self.upsert_grant(
            db,
            &application,
            &scope,
            normalized_kinds(environment_kinds)?,
            GrantBasis::PrivilegedOperator,
        )
        .await
    }

    async fn upsert_grant<C: ConnectionTrait>(
        &self,
        db: &C,
        application: &super::applications::ApplicationView,
        scope: &Scope,
        kinds: Option<Vec<String>>,
        basis: GrantBasis,
    ) -> PlatformResult<ApplicationScopeView> {
        let now = self.clock.now();
        application_scopes::Entity::insert(application_scopes::ActiveModel {
            application_id: Set(application.id.as_uuid()),
            scope: Set(scope.as_str().to_string()),
            tenant_id: Set(application.tenant_id.as_uuid()),
            allowed_environment_kinds: Set(kinds),
            grant_basis: Set(basis.as_str().to_string()),
            granted_at: Set(now),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                application_scopes::Column::ApplicationId,
                application_scopes::Column::Scope,
            ])
            .update_columns([
                application_scopes::Column::AllowedEnvironmentKinds,
                application_scopes::Column::GrantBasis,
            ])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
        let row = application_scopes::Entity::find_by_id((
            application.id.as_uuid(),
            scope.as_str().to_string(),
        ))
        .one(db)
        .await?
        .ok_or(PlatformError::NotFound("application scope"))?;
        tracing::info!(
            application_id = %application.id,
            scope = scope.as_str(),
            basis = basis.as_str(),
            "application scope allowed"
        );
        Ok(view(row))
    }

    /// Removes `scope` from the application's allowed set and, in the same statement
    /// (foreign-key cascade), from every credential of the application. Idempotent.
    /// Returns whether the scope was allowed before.
    pub async fn revoke_application_scope(
        &self,
        application_id: ApplicationId,
        scope: &str,
    ) -> PlatformResult<bool> {
        self.revoke_application_scope_in(&self.db, application_id, scope)
            .await
    }

    pub(crate) async fn revoke_application_scope_in<C: ConnectionTrait>(
        &self,
        db: &C,
        application_id: ApplicationId,
        scope: &str,
    ) -> PlatformResult<bool> {
        self.application(application_id).await?;
        let result = application_scopes::Entity::delete_many()
            .filter(application_scopes::Column::ApplicationId.eq(application_id.as_uuid()))
            .filter(application_scopes::Column::Scope.eq(scope))
            .exec(db)
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
            .map(view)
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

    fn grant(scope: &str, basis: GrantBasis, environment_allowed: bool) -> PersistedGrant {
        PersistedGrant {
            scope: scope.to_string(),
            grant_basis: basis,
            environment_allowed,
        }
    }

    #[test]
    fn effective_scopes_drop_unknown_and_untrusted_grants() {
        let registry = ScopeRegistry::standard();
        let set = registry.effective(
            vec![
                grant(names::PRINCIPALS_READ, GrantBasis::Standard, true),
                grant("legacy.removed", GrantBasis::Standard, true),
                grant(names::EVENTS_PRIVATE_DELIVERY, GrantBasis::Standard, true),
            ],
            TenantClassification::External,
        );
        assert_eq!(set.names(), vec![names::PRINCIPALS_READ.to_string()]);
    }

    #[test]
    fn numbering_and_sms_scopes_are_registered() {
        let registry = ScopeRegistry::standard();
        for scope in [
            names::NUMBERS_SEARCH,
            names::NUMBERS_READ,
            names::NUMBERS_MANAGE,
            names::NUMBERS_ASSIGN,
            names::NUMBERS_KYC_READ,
            names::NUMBERS_KYC_MANAGE,
            names::NUMBERS_PORTABILITY,
            names::SMS_SEND,
            names::SMS_READ,
        ] {
            for classification in TenantClassification::ALL {
                assert!(
                    registry.grantable(scope, *classification).is_ok(),
                    "{scope}"
                );
            }
        }
        assert!(registry.definition(names::NUMBERS_ADMIN).is_some());
    }

    #[test]
    fn numbers_admin_is_privileged_operator_authority() {
        let registry = ScopeRegistry::standard();
        for classification in TenantClassification::ALL {
            assert!(matches!(
                registry.grantable(names::NUMBERS_ADMIN, *classification),
                Err(PlatformError::PolicyViolation(_))
            ));
        }
        assert!(registry
            .grantable_privileged(names::NUMBERS_ADMIN, TenantClassification::FirstParty)
            .is_ok());
        assert!(registry
            .grantable_privileged(names::NUMBERS_ADMIN, TenantClassification::External)
            .is_err());
        assert!(registry
            .grantable_privileged(names::NUMBERS_READ, TenantClassification::FirstParty)
            .is_err());
        // Honoured only with the privileged basis, for FIRST_PARTY, in an allowed env.
        let ok = grant(names::NUMBERS_ADMIN, GrantBasis::PrivilegedOperator, true);
        assert!(registry.honours(&ok, TenantClassification::FirstParty));
        assert!(!registry.honours(&ok, TenantClassification::External));
        assert!(!registry.honours(
            &grant(names::NUMBERS_ADMIN, GrantBasis::Standard, true),
            TenantClassification::FirstParty
        ));
        assert!(!registry.honours(
            &grant(names::NUMBERS_ADMIN, GrantBasis::PrivilegedOperator, false),
            TenantClassification::FirstParty
        ));
    }

    #[test]
    fn environment_restricted_grants_are_not_honoured_elsewhere() {
        let registry = ScopeRegistry::standard();
        let set = registry.effective(
            vec![
                grant(names::NUMBERS_MANAGE, GrantBasis::Standard, false),
                grant(names::NUMBERS_READ, GrantBasis::Standard, true),
            ],
            TenantClassification::External,
        );
        assert_eq!(set.names(), vec![names::NUMBERS_READ.to_string()]);
        let only_sandbox = AllowedScope {
            environment_kinds: Some(vec![EnvironmentKind::Sandbox]),
            grant_basis: GrantBasis::Standard,
        };
        assert!(only_sandbox.allows(EnvironmentKind::Sandbox));
        assert!(!only_sandbox.allows(EnvironmentKind::Production));
        assert!(normalized_kinds(Some(&[])).is_err());
    }
}
