//! Call forwarding and protected public destinations (Part F §58, §85–§88).
//!
//! * A **public contact** is an admin-registered PROTECTED public destination: an opaque
//!   Privacy Relay ContactReference and the Privacy Relay Session whose authority covers
//!   it. Calling never stores, sees or accepts a telephone number; the contact's number
//!   is resolved only by Telephony, through Privacy Relay / Contact Vault, when a Call is
//!   actually forwarded — after every Part E gate (§87).
//! * A **forwarding rule** forwards a principal's calls under one condition (ALWAYS, BUSY,
//!   NO_ANSWER, UNAVAILABLE) to a typed destination (a principal, an extension, a ring
//!   group, a queue, an IVR or a public contact). At most one ACTIVE rule per principal
//!   and condition.
//! * Loops are impossible at routing time (visited set + hop budget, `router`); obvious
//!   ALWAYS cycles between principals are also refused when configured.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    departments::active_principal,
    destinations::{self, Destination, DestinationType},
    exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

/// How far a configuration-time ALWAYS chain is followed (runtime is hop-budgeted).
const MAX_CHAIN: usize = 16;

text_enum!(
    /// When a principal's forwarding applies: ALWAYS (before ringing), BUSY (every ring
    /// offer declined), NO_ANSWER (offers expired), UNAVAILABLE (no eligible endpoint).
    ForwardingCondition {
        Always = "ALWAYS",
        Busy = "BUSY",
        NoAnswer = "NO_ANSWER",
        Unavailable = "UNAVAILABLE",
    }
);

/// Kinds a forwarding rule may target (as configured kinds become available).
pub fn forwarding_types() -> &'static [DestinationType] {
    &[
        DestinationType::EnterpriseMember,
        DestinationType::Principal,
        DestinationType::Extension,
        DestinationType::RingGroup,
        DestinationType::Queue,
        DestinationType::Ivr,
        DestinationType::Voicemail,
        DestinationType::PublicContact,
    ]
}

// ---------------------------------------------------------------------------
// Public contacts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct PublicContactRow {
    pub id: Uuid,
    pub label: String,
    pub contact_reference: Uuid,
    pub privacy_relay_session_id: Uuid,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

const CONTACT_COLUMNS: &str = "id, label, contact_reference, privacy_relay_session_id, status, \
    version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicContactView {
    pub public_contact_id: Uuid,
    pub label: String,
    /// Opaque Privacy Relay ContactReference (never a number).
    pub contact_reference: Uuid,
    pub privacy_relay_session_id: Uuid,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicContactPage {
    pub public_contacts: Vec<PublicContactView>,
    pub next_cursor: Option<String>,
}

fn contact_view(row: PublicContactRow) -> PublicContactView {
    PublicContactView {
        public_contact_id: row.id,
        label: row.label,
        contact_reference: row.contact_reference,
        privacy_relay_session_id: row.privacy_relay_session_id,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    }
}

pub(crate) async fn public_contact_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<PublicContactRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    PublicContactRow::find_by_statement(stmt(
        &format!(
            "SELECT {CONTACT_COLUMNS} FROM enterprise_public_contacts \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// Public forwarding is configurable only where the enterprise policy permits it and the
/// organization granted its administrators MANAGE_PUBLIC_FORWARDING.
async fn require_public_forwarding<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
) -> EnterpriseResult<()> {
    policy::require_capability(db, scope, AdminCapability::ManagePublicForwarding).await?;
    let rules = policy::effective_policy(db, scope, None).await?.rules;
    if rules.forwarding_allowed && rules.public_forwarding_allowed {
        Ok(())
    } else {
        Err(EnterpriseError::PolicyDenied(
            "The enterprise policy does not permit public forwarding.",
        ))
    }
}

// ---------------------------------------------------------------------------
// Forwarding rules
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct ForwardingRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub condition: String,
    pub destination_type: String,
    pub destination_id: Uuid,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl ForwardingRow {
    pub(crate) fn destination(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(&self.destination_type)?,
            self.destination_id,
        ))
    }
}

const RULE_COLUMNS: &str = "id, principal_id, condition, destination_type, destination_id, \
    status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingRuleView {
    pub forwarding_rule_id: Uuid,
    pub principal_id: Uuid,
    pub condition: String,
    pub destination: Option<Destination>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingRulePage {
    pub forwarding_rules: Vec<ForwardingRuleView>,
    pub next_cursor: Option<String>,
}

fn rule_view(row: ForwardingRow) -> ForwardingRuleView {
    ForwardingRuleView {
        destination: row.destination(),
        forwarding_rule_id: row.id,
        principal_id: row.principal_id,
        condition: row.condition,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    }
}

async fn rule_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<ForwardingRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    ForwardingRow::find_by_statement(stmt(
        &format!(
            "SELECT {RULE_COLUMNS} FROM enterprise_forwarding_rules \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The ACTIVE rule of `principal` for `condition` in `scope`.
pub(crate) async fn active_rule<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    principal: Uuid,
    condition: ForwardingCondition,
) -> EnterpriseResult<Option<ForwardingRow>> {
    let mut values = scope_values(scope);
    values.extend([principal.into(), condition.as_str().into()]);
    Ok(ForwardingRow::find_by_statement(stmt(
        &format!(
            "SELECT {RULE_COLUMNS} FROM enterprise_forwarding_rules \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
               AND principal_id = $4 AND condition = $5 AND status = 'ACTIVE'"
        ),
        values,
    ))
    .one(db)
    .await?)
}

/// Whether following ALWAYS rules from `target` comes back to `owner` (configuration-time
/// cycle check; the router's visited set stays the runtime guarantee).
async fn always_cycle<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    owner: Uuid,
    target: Destination,
) -> EnterpriseResult<bool> {
    let mut current = target;
    for _ in 0..MAX_CHAIN {
        if !current.kind.is_principal() {
            return Ok(false);
        }
        if current.id == owner {
            return Ok(true);
        }
        match active_rule(db, scope, current.id, ForwardingCondition::Always).await? {
            Some(rule) => match rule.destination() {
                Some(next) => current = next,
                None => return Ok(false),
            },
            None => return Ok(false),
        }
    }
    Ok(true)
}

impl EnterpriseService {
    pub async fn create_public_contact(
        &self,
        actor: &AdminActor,
        label: String,
        contact_reference: Uuid,
        privacy_relay_session_id: Uuid,
    ) -> EnterpriseResult<PublicContactView> {
        let label = clean_name(&label, 100, "label must be 1-100 characters.")?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        require_public_forwarding(&txn, actor.scope).await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            label.into(),
            contact_reference.into(),
            privacy_relay_session_id.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_public_contacts (id, tenant_id, application_id, environment_id, \
             label, contact_reference, privacy_relay_session_id, status, version, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, 'ACTIVE', 1, $8, $8)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::PublicContact,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = contact_view(public_contact_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "PUBLIC_CONTACT"), ("operation", "CREATE")],
        );
        Ok(out)
    }

    pub async fn get_public_contact(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<PublicContactView> {
        Ok(contact_view(
            public_contact_row(self.db(), scope, id, false).await?,
        ))
    }

    pub async fn list_public_contacts(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<PublicContactPage> {
        let (rows, next_cursor) = self
            .page_of::<PublicContactRow>(
                "enterprise_public_contacts",
                CONTACT_COLUMNS,
                scope,
                status,
                None,
                page,
            )
            .await?;
        Ok(PublicContactPage {
            public_contacts: rows.into_iter().map(contact_view).collect(),
            next_cursor,
        })
    }

    /// Never blocked: disabling a public destination stops new forwarding to it.
    pub async fn disable_public_contact(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<PublicContactView> {
        let txn = self.begin().await?;
        let row = public_contact_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the public contact is already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_public_contacts SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::PublicContact,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = contact_view(public_contact_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    async fn validate_forwarding_target(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        scope: CallScope,
        principal_id: Uuid,
        condition: ForwardingCondition,
        destination: Destination,
    ) -> EnterpriseResult<()> {
        let rules = policy::effective_policy(txn, scope, None).await?.rules;
        if !rules.forwarding_allowed {
            return Err(EnterpriseError::PolicyDenied(
                "The enterprise policy does not permit forwarding.",
            ));
        }
        if destination.kind.is_principal() && destination.id == principal_id {
            return Err(EnterpriseError::Validation(
                "a principal cannot forward to itself.",
            ));
        }
        destinations::validate(txn, scope, destination, forwarding_types()).await?;
        if destination.kind == DestinationType::PublicContact {
            require_public_forwarding(txn, scope).await?;
            let contact = public_contact_row(txn, scope, destination.id, false).await?;
            if contact.status != "ACTIVE" {
                return Err(EnterpriseError::NotFound);
            }
        }
        if condition == ForwardingCondition::Always
            && always_cycle(txn, scope, principal_id, destination).await?
        {
            return Err(EnterpriseError::Validation(
                "the forwarding would form a loop.",
            ));
        }
        Ok(())
    }

    pub async fn create_forwarding_rule(
        &self,
        actor: &AdminActor,
        principal_id: Uuid,
        condition: ForwardingCondition,
        destination: Destination,
    ) -> EnterpriseResult<ForwardingRuleView> {
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        if !active_principal(&txn, actor.scope, principal_id).await? {
            return Err(EnterpriseError::NotFound);
        }
        self.validate_forwarding_target(&txn, actor.scope, principal_id, condition, destination)
            .await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            principal_id.into(),
            condition.as_str().into(),
            destination.kind.as_str().into(),
            destination.id.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_forwarding_rules (id, tenant_id, application_id, environment_id, \
             principal_id, condition, destination_type, destination_id, status, version, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'ACTIVE', 1, $9, $9)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::ForwardingRule,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("condition", condition.as_str())
                    .with("destinationType", destination.kind.as_str()),
            },
            now,
        )
        .await?;
        let out = rule_view(rule_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "FORWARDING_RULE"), ("operation", "CREATE")],
        );
        Ok(out)
    }

    pub async fn get_forwarding_rule(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<ForwardingRuleView> {
        Ok(rule_view(rule_row(self.db(), scope, id, false).await?))
    }

    pub async fn list_forwarding_rules(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        principal: Option<Uuid>,
        page: &PageRequest,
    ) -> EnterpriseResult<ForwardingRulePage> {
        let (rows, next_cursor) = self
            .page_of::<ForwardingRow>(
                "enterprise_forwarding_rules",
                RULE_COLUMNS,
                scope,
                status,
                principal.map(|p| ("principal_id", p)),
                page,
            )
            .await?;
        Ok(ForwardingRulePage {
            forwarding_rules: rows.into_iter().map(rule_view).collect(),
            next_cursor,
        })
    }

    /// Changes the target of a rule (its principal and condition are fixed).
    pub async fn update_forwarding_rule(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        destination: Destination,
    ) -> EnterpriseResult<ForwardingRuleView> {
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = rule_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled forwarding rule cannot be changed",
            ));
        }
        let condition =
            ForwardingCondition::parse(&row.condition).unwrap_or(ForwardingCondition::Always);
        self.validate_forwarding_target(
            &txn,
            actor.scope,
            row.principal_id,
            condition,
            destination,
        )
        .await?;
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_forwarding_rules SET destination_type = $2, destination_id = $3, \
             version = version + 1, updated_at = GREATEST(updated_at, $4) WHERE id = $1",
            vec![
                id.into(),
                destination.kind.as_str().into(),
                destination.id.into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::ForwardingRule,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new().with("destinationType", destination.kind.as_str()),
            },
            now,
        )
        .await?;
        let out = rule_view(rule_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// Never blocked.
    pub async fn disable_forwarding_rule(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<ForwardingRuleView> {
        let txn = self.begin().await?;
        let row = rule_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the forwarding rule is already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_forwarding_rules SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::ForwardingRule,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = rule_view(rule_row(&txn, actor.scope, id, false).await?);
        txn.commit().await?;
        Ok(out)
    }

    /// One keyset page of a scope-confined table (`created_at, id` order).
    async fn page_of<R: FromQueryResult + Created>(
        &self,
        table: &str,
        columns: &str,
        scope: CallScope,
        status: Option<ResourceStatus>,
        filter: Option<(&str, Uuid)>,
        page: &PageRequest,
    ) -> EnterpriseResult<(Vec<R>, Option<String>)> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {columns} FROM {table} \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((column, id)) = filter {
            values.push(id.into());
            sql.push_str(&format!(" AND {column} = ${}", values.len()));
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
        let mut rows = R::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created(), r.id()))
        } else {
            None
        };
        Ok((rows, next_cursor))
    }
}

/// Keyset position of a row.
trait Created {
    fn created(&self) -> DateTime<Utc>;
    fn id(&self) -> Uuid;
}

impl Created for PublicContactRow {
    fn created(&self) -> DateTime<Utc> {
        self.created_at
    }
    fn id(&self) -> Uuid {
        self.id
    }
}

impl Created for ForwardingRow {
    fn created(&self) -> DateTime<Utc> {
        self.created_at
    }
    fn id(&self) -> Uuid {
        self.id
    }
}
