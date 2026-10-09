//! The typed Enterprise Call Policy (Part F §18).
//!
//! Every hook is a typed, validated field stored in its own column — never an arbitrary
//! policy blob made into authority. An environment has at most one ACTIVE default
//! policy; an extension may name another one. Without any stored policy the restrictive
//! built-in default applies: internal calling, transfer and voicemail allowed; no public
//! calling, no public forwarding; bounded fan-out, queues and routing hops.
//!
//! Administrator capabilities are an organization-level second key on top of the
//! credential's scopes: e.g. even a credential with `enterprise.config.write` cannot
//! configure public forwarding unless the environment's default policy grants
//! `MANAGE_PUBLIC_FORWARDING`.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name, exec, scope_values, stmt, time, AdminActor, EnterpriseError,
    EnterpriseResult, EnterpriseService, PageRequest,
};
use crate::calls::command::CallScope;

text_enum!(
    /// Lifecycle of a configuration resource. Configuration is never deleted: disabling
    /// stops future routing and keeps historical references valid.
    ResourceStatus {
        Active = "ACTIVE",
        Disabled = "DISABLED",
    }
);

text_enum!(
    /// Where public-network spend of enterprise calls is accounted: always the
    /// application's Part E telecom spend limits (one spend authority, referenced).
    SpendPolicy {
        ApplicationTelecomLimits = "APPLICATION_TELECOM_LIMITS",
    }
);

text_enum!(
    /// Extension number format of the namespace.
    ExtensionFormat {
        Numeric = "NUMERIC",
        Alphanumeric = "ALPHANUMERIC",
    }
);

text_enum!(
    /// Administrative capabilities the organization grants (second key after scopes).
    AdminCapability {
        ManageRouting = "MANAGE_ROUTING",
        ManagePublicForwarding = "MANAGE_PUBLIC_FORWARDING",
        ManageVoicemail = "MANAGE_VOICEMAIL",
        ManageRecording = "MANAGE_RECORDING",
    }
);

/// The policy rules (typed; unknown fields refused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyRules {
    pub internal_calling_enabled: bool,
    pub external_public_calling_allowed: bool,
    /// ISO 3166-1 alpha-2 countries enterprise public forwarding may reach (in ADDITION
    /// to the environment's routing policy, which still applies). Empty = none.
    #[serde(default)]
    pub public_destination_countries: Vec<String>,
    pub forwarding_allowed: bool,
    pub public_forwarding_allowed: bool,
    pub spend_policy: SpendPolicy,
    pub voicemail_allowed: bool,
    pub transfer_allowed: bool,
    pub max_ring_group_fanout: u8,
    pub max_queue_capacity: u32,
    pub max_queue_wait_seconds: u32,
    pub max_routing_hops: u8,
    pub extension_format: ExtensionFormat,
    pub extension_min_length: u8,
    pub extension_max_length: u8,
    #[serde(default)]
    pub admin_capabilities: Vec<AdminCapability>,
}

impl PolicyRules {
    /// The restrictive built-in default.
    pub fn builtin_default() -> Self {
        Self {
            internal_calling_enabled: true,
            external_public_calling_allowed: false,
            public_destination_countries: Vec::new(),
            forwarding_allowed: true,
            public_forwarding_allowed: false,
            spend_policy: SpendPolicy::ApplicationTelecomLimits,
            voicemail_allowed: true,
            transfer_allowed: true,
            max_ring_group_fanout: 10,
            max_queue_capacity: 50,
            max_queue_wait_seconds: 600,
            max_routing_hops: 8,
            extension_format: ExtensionFormat::Numeric,
            extension_min_length: 3,
            extension_max_length: 6,
            admin_capabilities: vec![
                AdminCapability::ManageRouting,
                AdminCapability::ManageVoicemail,
            ],
        }
    }

    pub fn validate(&self) -> EnterpriseResult<()> {
        if self.public_destination_countries.len() > 300
            || !self
                .public_destination_countries
                .iter()
                .all(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()))
        {
            return Err(EnterpriseError::Validation(
                "publicDestinationCountries must be ISO 3166-1 alpha-2 codes.",
            ));
        }
        let mut countries = self.public_destination_countries.clone();
        countries.sort();
        countries.dedup();
        if countries.len() != self.public_destination_countries.len() {
            return Err(EnterpriseError::Validation(
                "publicDestinationCountries must be distinct.",
            ));
        }
        if self.public_forwarding_allowed
            && !(self.forwarding_allowed && self.external_public_calling_allowed)
        {
            return Err(EnterpriseError::Validation(
                "publicForwardingAllowed requires forwardingAllowed and externalPublicCallingAllowed.",
            ));
        }
        if !(1..=15).contains(&self.max_ring_group_fanout) {
            return Err(EnterpriseError::Validation(
                "maxRingGroupFanout must be between 1 and 15.",
            ));
        }
        if !(1..=1000).contains(&self.max_queue_capacity) {
            return Err(EnterpriseError::Validation(
                "maxQueueCapacity must be between 1 and 1000.",
            ));
        }
        if !(10..=3600).contains(&self.max_queue_wait_seconds) {
            return Err(EnterpriseError::Validation(
                "maxQueueWaitSeconds must be between 10 and 3600.",
            ));
        }
        if !(1..=16).contains(&self.max_routing_hops) {
            return Err(EnterpriseError::Validation(
                "maxRoutingHops must be between 1 and 16.",
            ));
        }
        let max = match self.extension_format {
            ExtensionFormat::Numeric => 6,
            ExtensionFormat::Alphanumeric => 32,
        };
        if self.extension_min_length < 2
            || self.extension_min_length > self.extension_max_length
            || self.extension_max_length > max
        {
            return Err(EnterpriseError::Validation(
                "extension lengths must satisfy 2 <= min <= max (max 6 numeric, 32 alphanumeric).",
            ));
        }
        let mut caps = self.admin_capabilities.clone();
        caps.sort();
        caps.dedup();
        if caps.len() != self.admin_capabilities.len() {
            return Err(EnterpriseError::Validation(
                "adminCapabilities must be distinct.",
            ));
        }
        Ok(())
    }

    pub fn grants(&self, capability: AdminCapability) -> bool {
        self.admin_capabilities.contains(&capability)
    }
}

/// The policy in force for a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectivePolicy {
    /// `None` = the built-in default.
    pub policy_id: Option<Uuid>,
    /// 0 = the built-in default.
    pub version: i64,
    pub rules: PolicyRules,
}

impl EffectivePolicy {
    pub fn builtin() -> Self {
        Self {
            policy_id: None,
            version: 0,
            rules: PolicyRules::builtin_default(),
        }
    }

    /// Stable label recorded on routes and authorizations.
    pub fn label(&self) -> String {
        match self.policy_id {
            None => "ENTERPRISE_POLICY.default".to_string(),
            Some(id) => format!("ENTERPRISE_POLICY.{}.v{}", id.simple(), self.version),
        }
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PolicyRow {
    pub id: Uuid,
    pub name: String,
    pub is_default: bool,
    pub status: String,
    pub internal_calling_enabled: bool,
    pub external_public_calling_allowed: bool,
    pub public_destination_countries: Vec<String>,
    pub forwarding_allowed: bool,
    pub public_forwarding_allowed: bool,
    pub spend_policy: String,
    pub voicemail_allowed: bool,
    pub transfer_allowed: bool,
    pub max_ring_group_fanout: i16,
    pub max_queue_capacity: i32,
    pub max_queue_wait_seconds: i32,
    pub max_routing_hops: i16,
    pub extension_format: String,
    pub extension_min_length: i16,
    pub extension_max_length: i16,
    pub admin_capabilities: Vec<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, name, is_default, status, internal_calling_enabled, \
    external_public_calling_allowed, public_destination_countries, forwarding_allowed, \
    public_forwarding_allowed, spend_policy, voicemail_allowed, transfer_allowed, \
    max_ring_group_fanout, max_queue_capacity, max_queue_wait_seconds, max_routing_hops, \
    extension_format, extension_min_length, extension_max_length, admin_capabilities, version, \
    created_at, updated_at, disabled_at";

impl PolicyRow {
    pub(crate) fn rules(&self) -> EnterpriseResult<PolicyRules> {
        let corrupt = || EnterpriseError::Database {
            summary: "stored enterprise policy does not parse".to_string(),
            connectivity: false,
        };
        Ok(PolicyRules {
            internal_calling_enabled: self.internal_calling_enabled,
            external_public_calling_allowed: self.external_public_calling_allowed,
            public_destination_countries: self.public_destination_countries.clone(),
            forwarding_allowed: self.forwarding_allowed,
            public_forwarding_allowed: self.public_forwarding_allowed,
            spend_policy: SpendPolicy::parse(&self.spend_policy).ok_or_else(corrupt)?,
            voicemail_allowed: self.voicemail_allowed,
            transfer_allowed: self.transfer_allowed,
            max_ring_group_fanout: self.max_ring_group_fanout as u8,
            max_queue_capacity: self.max_queue_capacity as u32,
            max_queue_wait_seconds: self.max_queue_wait_seconds as u32,
            max_routing_hops: self.max_routing_hops as u8,
            extension_format: ExtensionFormat::parse(&self.extension_format).ok_or_else(corrupt)?,
            extension_min_length: self.extension_min_length as u8,
            extension_max_length: self.extension_max_length as u8,
            admin_capabilities: self
                .admin_capabilities
                .iter()
                .map(|c| AdminCapability::parse(c).ok_or_else(corrupt))
                .collect::<EnterpriseResult<_>>()?,
        })
    }
}

/// Public view of a stored policy.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallPolicyView {
    pub policy_id: Uuid,
    pub name: String,
    pub is_default: bool,
    pub status: String,
    pub rules: PolicyRules,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

impl TryFrom<PolicyRow> for CallPolicyView {
    type Error = EnterpriseError;

    fn try_from(r: PolicyRow) -> EnterpriseResult<Self> {
        Ok(Self {
            rules: r.rules()?,
            policy_id: r.id,
            name: r.name,
            is_default: r.is_default,
            status: r.status,
            version: r.version,
            created_at: time(r.created_at),
            updated_at: time(r.updated_at),
            disabled_at: r.disabled_at.map(time),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallPolicyPage {
    pub policies: Vec<CallPolicyView>,
    pub next_cursor: Option<String>,
}

/// A new policy.
#[derive(Debug, Clone)]
pub struct NewPolicy {
    pub name: String,
    pub is_default: bool,
    pub rules: PolicyRules,
}

/// A policy change (absent fields keep their value; `rules` replaces the rule set).
#[derive(Debug, Clone, Default)]
pub struct PolicyChange {
    pub name: Option<String>,
    pub is_default: Option<bool>,
    pub rules: Option<PolicyRules>,
}

/// Loads a policy row of `scope`, optionally locking it.
pub(crate) async fn policy_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<PolicyRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    PolicyRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_call_policies \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The policy in force: `policy_id` if given and ACTIVE in `scope`, else the
/// environment's ACTIVE default, else the built-in default. A named policy that is
/// disabled (or foreign) is never silently replaced by a more permissive one: it
/// falls back to the default, which is the organization's baseline.
pub(crate) async fn effective_policy<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    policy_id: Option<Uuid>,
) -> EnterpriseResult<EffectivePolicy> {
    if let Some(id) = policy_id {
        match policy_row(db, scope, id, false).await {
            Ok(row) if row.status == "ACTIVE" => {
                return Ok(EffectivePolicy {
                    policy_id: Some(row.id),
                    version: row.version,
                    rules: row.rules()?,
                })
            }
            Ok(_) | Err(EnterpriseError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    let row = PolicyRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_call_policies \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
             AND is_default AND status = 'ACTIVE'"
        ),
        scope_values(scope),
    ))
    .one(db)
    .await?;
    Ok(match row {
        Some(row) => EffectivePolicy {
            policy_id: Some(row.id),
            version: row.version,
            rules: row.rules()?,
        },
        None => EffectivePolicy::builtin(),
    })
}

/// Refuses an administrative operation the organization's baseline policy does not
/// grant (second key after the credential scope).
pub(crate) async fn require_capability<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    capability: AdminCapability,
) -> EnterpriseResult<()> {
    if effective_policy(db, scope, None)
        .await?
        .rules
        .grants(capability)
    {
        Ok(())
    } else {
        Err(EnterpriseError::PolicyDenied(
            "the enterprise policy does not grant this administrative capability",
        ))
    }
}

fn rule_values(rules: &PolicyRules) -> Vec<sea_orm::Value> {
    vec![
        rules.internal_calling_enabled.into(),
        rules.external_public_calling_allowed.into(),
        rules.public_destination_countries.clone().into(),
        rules.forwarding_allowed.into(),
        rules.public_forwarding_allowed.into(),
        rules.spend_policy.as_str().into(),
        rules.voicemail_allowed.into(),
        rules.transfer_allowed.into(),
        (rules.max_ring_group_fanout as i16).into(),
        (rules.max_queue_capacity as i32).into(),
        (rules.max_queue_wait_seconds as i32).into(),
        (rules.max_routing_hops as i16).into(),
        rules.extension_format.as_str().into(),
        (rules.extension_min_length as i16).into(),
        (rules.extension_max_length as i16).into(),
        rules
            .admin_capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect::<Vec<_>>()
            .into(),
    ]
}

fn audit_details(rules: &PolicyRules, is_default: bool) -> AuditDetails {
    AuditDetails::new()
        .with("is_default", is_default)
        .with("internal_calling_enabled", rules.internal_calling_enabled)
        .with(
            "external_public_calling_allowed",
            rules.external_public_calling_allowed,
        )
        .with("forwarding_allowed", rules.forwarding_allowed)
        .with("public_forwarding_allowed", rules.public_forwarding_allowed)
        .with("voicemail_allowed", rules.voicemail_allowed)
        .with("transfer_allowed", rules.transfer_allowed)
        .with("admin_capability_count", rules.admin_capabilities.len())
}

impl EnterpriseService {
    pub async fn create_policy(
        &self,
        actor: &AdminActor,
        new: NewPolicy,
    ) -> EnterpriseResult<CallPolicyView> {
        let name = clean_name(&new.name, 100, "name must be 1-100 characters.")?;
        new.rules.validate()?;
        let txn = self.begin().await?;
        let current = effective_policy(&txn, actor.scope, None).await?;
        // Granting the public / recording capabilities, or enabling public forwarding, is
        // itself gated by the organization's current baseline.
        guard_escalation(&current.rules, &new.rules)?;
        let now = self.now();
        let id = Uuid::new_v4();
        if new.is_default {
            self.clear_default(&txn, actor.scope).await?;
        }
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([name.clone().into(), new.is_default.into()]);
        values.extend(rule_values(&new.rules));
        values.push(now.into());
        exec(
            &txn,
            "INSERT INTO enterprise_call_policies (id, tenant_id, application_id, environment_id, \
             name, is_default, status, internal_calling_enabled, external_public_calling_allowed, \
             public_destination_countries, forwarding_allowed, public_forwarding_allowed, \
             spend_policy, voicemail_allowed, transfer_allowed, max_ring_group_fanout, \
             max_queue_capacity, max_queue_wait_seconds, max_routing_hops, extension_format, \
             extension_min_length, extension_max_length, admin_capabilities, version, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', $7, $8, $9, $10, $11, $12, $13, \
             $14, $15, $16, $17, $18, $19, $20, $21, $22, 1, $23, $23)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::CallPolicy,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: audit_details(&new.rules, new.is_default),
            },
            now,
        )
        .await?;
        let row = policy_row(&txn, actor.scope, id, false).await?;
        txn.commit().await?;
        row.try_into()
    }

    async fn clear_default<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
    ) -> EnterpriseResult<()> {
        let mut values = scope_values(scope);
        values.push(self.now().into());
        exec(
            db,
            "UPDATE enterprise_call_policies SET is_default = FALSE, version = version + 1, \
             updated_at = GREATEST(updated_at, $4) \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND is_default",
            values,
        )
        .await?;
        Ok(())
    }

    pub async fn get_policy(&self, scope: CallScope, id: Uuid) -> EnterpriseResult<CallPolicyView> {
        policy_row(self.db(), scope, id, false).await?.try_into()
    }

    /// The policy in force for the environment (default or built-in).
    pub async fn effective_default_policy(
        &self,
        scope: CallScope,
    ) -> EnterpriseResult<EffectivePolicy> {
        effective_policy(self.db(), scope, None).await
    }

    pub async fn list_policies(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<CallPolicyPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_call_policies \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = PolicyRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        Ok(CallPolicyPage {
            policies: rows
                .into_iter()
                .map(CallPolicyView::try_from)
                .collect::<EnterpriseResult<_>>()?,
            next_cursor,
        })
    }

    pub async fn update_policy(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: PolicyChange,
    ) -> EnterpriseResult<CallPolicyView> {
        let name = change
            .name
            .as_deref()
            .map(|n| clean_name(n, 100, "name must be 1-100 characters."))
            .transpose()?;
        if let Some(rules) = &change.rules {
            rules.validate()?;
        }
        let txn = self.begin().await?;
        // Default changes serialize per environment (one default at a time).
        exec(
            &txn,
            "SELECT pg_advisory_xact_lock(hashtextextended('enterprise-policy-default:' || $1::text, 0))",
            vec![actor.scope.environment_id().as_uuid().into()],
        )
        .await?;
        let row = policy_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled policy cannot be changed",
            ));
        }
        let rules = match change.rules {
            Some(rules) => {
                let baseline = effective_policy(&txn, actor.scope, None).await?;
                guard_escalation(&baseline.rules, &rules)?;
                rules
            }
            None => row.rules()?,
        };
        let is_default = change.is_default.unwrap_or(row.is_default);
        let now = self.now();
        if is_default && !row.is_default {
            self.clear_default(&txn, actor.scope).await?;
        }
        let mut values = vec![
            id.into(),
            name.clone().unwrap_or(row.name.clone()).into(),
            is_default.into(),
        ];
        values.extend(rule_values(&rules));
        values.push(now.into());
        exec(
            &txn,
            "UPDATE enterprise_call_policies SET name = $2, is_default = $3, \
             internal_calling_enabled = $4, external_public_calling_allowed = $5, \
             public_destination_countries = $6, forwarding_allowed = $7, \
             public_forwarding_allowed = $8, spend_policy = $9, voicemail_allowed = $10, \
             transfer_allowed = $11, max_ring_group_fanout = $12, max_queue_capacity = $13, \
             max_queue_wait_seconds = $14, max_routing_hops = $15, extension_format = $16, \
             extension_min_length = $17, extension_max_length = $18, admin_capabilities = $19, \
             version = version + 1, updated_at = GREATEST(updated_at, $20) WHERE id = $1",
            values,
        )
        .await?;
        let updated = policy_row(&txn, actor.scope, id, false).await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::CallPolicy,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(updated.version),
                details: audit_details(&rules, is_default),
            },
            now,
        )
        .await?;
        txn.commit().await?;
        updated.try_into()
    }

    /// Disables a policy (never deleted). Disabling is a restrictive operation and is
    /// never blocked by a kill switch.
    pub async fn disable_policy(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<CallPolicyView> {
        let txn = self.begin().await?;
        let row = policy_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict("the policy is already disabled"));
        }
        // An extension naming this policy would silently change policy.
        if super::extensions::policy_in_use(&txn, actor.scope, id).await? {
            return Err(EnterpriseError::Conflict(
                "the policy is still named by an active extension",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_call_policies SET status = 'DISABLED', is_default = FALSE, \
             disabled_at = $2, version = version + 1, updated_at = GREATEST(updated_at, $2) \
             WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::CallPolicy,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let updated = policy_row(&txn, actor.scope, id, false).await?;
        txn.commit().await?;
        updated.try_into()
    }
}

impl EnterpriseService {
    /// OPERATOR ONLY (`platform_admin`): sets the administrative capabilities of the
    /// environment's baseline (default) policy, creating the baseline from the built-in
    /// default when none is stored. High-risk capabilities (public forwarding, recording
    /// administration) can only be granted here — never through the customer API.
    pub async fn operator_set_capabilities(
        &self,
        scope: CallScope,
        capabilities: Vec<AdminCapability>,
    ) -> EnterpriseResult<CallPolicyView> {
        let actor = AdminActor::operator(scope);
        let mut caps = capabilities.clone();
        caps.sort();
        caps.dedup();
        if caps.len() != capabilities.len() {
            return Err(EnterpriseError::Validation(
                "capabilities must be distinct.",
            ));
        }
        let txn = self.begin().await?;
        exec(
            &txn,
            "SELECT pg_advisory_xact_lock(hashtextextended('enterprise-policy-default:' || $1::text, 0))",
            vec![scope.environment_id().as_uuid().into()],
        )
        .await?;
        let current = effective_policy(&txn, scope, None).await?;
        let now = self.now();
        let caps_value: Vec<String> = caps.iter().map(|c| c.as_str().to_string()).collect();
        let id = match current.policy_id {
            Some(id) => {
                exec(
                    &txn,
                    "UPDATE enterprise_call_policies SET admin_capabilities = $2, \
                     version = version + 1, updated_at = GREATEST(updated_at, $3) WHERE id = $1",
                    vec![id.into(), caps_value.into(), now.into()],
                )
                .await?;
                audit::record(
                    &txn,
                    &actor,
                    AuditFact {
                        resource: AuditResource::CallPolicy,
                        resource_id: id,
                        operation: AuditOperation::Update,
                        old_version: Some(current.version),
                        new_version: Some(current.version + 1),
                        details: AuditDetails::new()
                            .with("admin_capability_count", caps.len())
                            .with("operator_grant", true),
                    },
                    now,
                )
                .await?;
                id
            }
            None => {
                let mut rules = PolicyRules::builtin_default();
                rules.admin_capabilities = caps.clone();
                let id = Uuid::new_v4();
                let mut values = vec![id.into()];
                values.extend(scope_values(scope));
                values.extend(["Organization baseline".into(), true.into()]);
                values.extend(rule_values(&rules));
                values.push(now.into());
                exec(
                    &txn,
                    "INSERT INTO enterprise_call_policies (id, tenant_id, application_id, environment_id, \
                     name, is_default, status, internal_calling_enabled, external_public_calling_allowed, \
                     public_destination_countries, forwarding_allowed, public_forwarding_allowed, \
                     spend_policy, voicemail_allowed, transfer_allowed, max_ring_group_fanout, \
                     max_queue_capacity, max_queue_wait_seconds, max_routing_hops, extension_format, \
                     extension_min_length, extension_max_length, admin_capabilities, version, created_at, \
                     updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'ACTIVE', $7, $8, $9, $10, $11, $12, $13, \
                     $14, $15, $16, $17, $18, $19, $20, $21, $22, 1, $23, $23)",
                    values,
                )
                .await?;
                audit::record(
                    &txn,
                    &actor,
                    AuditFact {
                        resource: AuditResource::CallPolicy,
                        resource_id: id,
                        operation: AuditOperation::Create,
                        old_version: None,
                        new_version: Some(1),
                        details: audit_details(&rules, true).with("operator_grant", true),
                    },
                    now,
                )
                .await?;
                id
            }
        };
        let row = policy_row(&txn, scope, id, false).await?;
        txn.commit().await?;
        row.try_into()
    }
}

/// A policy change may not grant what the current organization baseline does not
/// permit an administrator to configure.
fn guard_escalation(baseline: &PolicyRules, rules: &PolicyRules) -> EnterpriseResult<()> {
    if (rules.public_forwarding_allowed || rules.grants(AdminCapability::ManagePublicForwarding))
        && !baseline.grants(AdminCapability::ManagePublicForwarding)
    {
        return Err(EnterpriseError::PolicyDenied(
            "public forwarding is not an administrative capability of this organization",
        ));
    }
    if rules.grants(AdminCapability::ManageRecording)
        && !baseline.grants(AdminCapability::ManageRecording)
    {
        return Err(EnterpriseError::PolicyDenied(
            "recording administration is not a capability of this organization",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_default_is_valid_and_restrictive() {
        let d = PolicyRules::builtin_default();
        d.validate().unwrap();
        assert!(!d.external_public_calling_allowed && !d.public_forwarding_allowed);
        assert!(!d.grants(AdminCapability::ManagePublicForwarding));
        assert!(!d.grants(AdminCapability::ManageRecording));
    }

    #[test]
    fn invalid_rules_are_refused() {
        let mut r = PolicyRules::builtin_default();
        r.public_forwarding_allowed = true;
        assert!(r.validate().is_err());
        let mut r = PolicyRules::builtin_default();
        r.max_ring_group_fanout = 0;
        assert!(r.validate().is_err());
        let mut r = PolicyRules::builtin_default();
        r.extension_max_length = 7;
        assert!(r.validate().is_err());
        let mut r = PolicyRules::builtin_default();
        r.public_destination_countries = vec!["ng".into()];
        assert!(r.validate().is_err());
        let mut r = PolicyRules::builtin_default();
        r.admin_capabilities = vec![
            AdminCapability::ManageRouting,
            AdminCapability::ManageRouting,
        ];
        assert!(r.validate().is_err());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut v = serde_json::to_value(PolicyRules::builtin_default()).unwrap();
        v["recordEverything"] = serde_json::json!(true);
        assert!(serde_json::from_value::<PolicyRules>(v).is_err());
    }

    #[test]
    fn escalation_needs_the_baseline_capability() {
        let base = PolicyRules::builtin_default();
        let mut wants = base.clone();
        wants.external_public_calling_allowed = true;
        wants.public_forwarding_allowed = true;
        assert!(guard_escalation(&base, &wants).is_err());
        let mut granted = base.clone();
        granted
            .admin_capabilities
            .push(AdminCapability::ManagePublicForwarding);
        assert!(guard_escalation(&granted, &wants).is_ok());
    }
}
