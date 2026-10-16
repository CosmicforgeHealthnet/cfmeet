//! Enterprise extensions (Part F §54, §26–§30, §34).
//!
//! An extension is an internal, organization-scoped routable identity: a short code such
//! as `1001`, `201` or `sales` — never a public telephone number. Numbers are unique
//! among ACTIVE extensions of one tenant / application / environment (not globally),
//! normalized to lower case and validated against the governing policy's format. Numeric
//! extensions may not be prefixes of one another (overlap dialing would be ambiguous).
//!
//! Offboarding disables an extension: future routing stops; the row, and every
//! historical Call's reference and number / label snapshot, remain.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::Serialize;
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    destinations::{self, Destination, DestinationType},
    exec,
    policy::{self, AdminCapability, ExtensionFormat, PolicyRules, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct ExtensionRow {
    pub id: Uuid,
    pub extension_number: String,
    pub display_label: String,
    pub destination_type: String,
    pub destination_id: Uuid,
    pub policy_id: Option<Uuid>,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl ExtensionRow {
    pub(crate) fn destination(&self) -> EnterpriseResult<Destination> {
        Ok(Destination::new(
            DestinationType::parse(&self.destination_type).ok_or(EnterpriseError::Database {
                summary: "stored extension destination does not parse".to_string(),
                connectivity: false,
            })?,
            self.destination_id,
        ))
    }
}

const COLUMNS: &str = "id, extension_number, display_label, destination_type, destination_id, \
    policy_id, status, version, created_at, updated_at, disabled_at";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionView {
    pub extension_id: Uuid,
    pub extension_number: String,
    pub display_label: String,
    pub destination: Destination,
    pub policy_id: Option<Uuid>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

impl TryFrom<ExtensionRow> for ExtensionView {
    type Error = EnterpriseError;

    fn try_from(r: ExtensionRow) -> EnterpriseResult<Self> {
        Ok(Self {
            destination: r.destination()?,
            extension_id: r.id,
            extension_number: r.extension_number,
            display_label: r.display_label,
            policy_id: r.policy_id,
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
pub struct ExtensionPage {
    pub extensions: Vec<ExtensionView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewExtension {
    pub extension_number: String,
    pub display_label: String,
    pub destination: Destination,
    pub policy_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionChange {
    pub extension_number: Option<String>,
    pub display_label: Option<String>,
    pub destination: Option<Destination>,
    /// `Some(None)` = use the environment default policy.
    pub policy_id: Option<Option<Uuid>>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtensionFilter {
    pub status: Option<ResourceStatus>,
    pub destination_type: Option<DestinationType>,
}

/// Normalizes and validates an extension number against the governing policy.
pub fn normalize_number(raw: &str, rules: &PolicyRules) -> EnterpriseResult<String> {
    let number = raw.trim().to_ascii_lowercase();
    let len = number.chars().count();
    let in_range =
        (rules.extension_min_length as usize..=rules.extension_max_length as usize).contains(&len);
    let well_formed = match rules.extension_format {
        ExtensionFormat::Numeric => number.bytes().all(|b| b.is_ascii_digit()),
        ExtensionFormat::Alphanumeric => {
            number
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
                && number
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        }
    };
    // Never a public telephone number: no '+', at most six digits in total.
    let digits = number.bytes().filter(u8::is_ascii_digit).count();
    if !in_range || !well_formed || digits > 6 || super::carries_dialable_number(&number) {
        return Err(EnterpriseError::Validation(
            "extensionNumber does not match the enterprise extension format.",
        ));
    }
    Ok(number)
}

pub(crate) async fn extension_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<ExtensionRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    ExtensionRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_extensions \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The ACTIVE extension `number` of `scope` (the scope IS the namespace: the same
/// number of another tenant / application / environment is invisible).
pub(crate) async fn resolve_number<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    number: &str,
) -> EnterpriseResult<Option<ExtensionRow>> {
    let mut values = scope_values(scope);
    values.push(number.trim().to_ascii_lowercase().into());
    Ok(ExtensionRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_extensions \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
             AND extension_number = $4 AND status = 'ACTIVE'"
        ),
        values,
    ))
    .one(db)
    .await?)
}

/// Whether an ACTIVE extension names policy `id`.
pub(crate) async fn policy_in_use<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
) -> EnterpriseResult<bool> {
    let mut values = scope_values(scope);
    values.push(id.into());
    Ok(db
        .query_one(stmt(
            "SELECT 1 FROM enterprise_extensions WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND policy_id = $4 AND status = 'ACTIVE' LIMIT 1",
            values,
        ))
        .await?
        .is_some())
}

/// Serializes numbering changes of one namespace.
async fn lock_namespace<C: ConnectionTrait>(db: &C, scope: CallScope) -> EnterpriseResult<()> {
    exec(
        db,
        "SELECT pg_advisory_xact_lock(hashtextextended('enterprise-extensions:' || $1::text, 0))",
        vec![scope.environment_id().as_uuid().into()],
    )
    .await?;
    Ok(())
}

/// Refuses a number that is taken or that is ambiguous with another ACTIVE numeric
/// extension (one a prefix of the other).
async fn check_namespace<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    number: &str,
    except: Option<Uuid>,
) -> EnterpriseResult<()> {
    let mut values = scope_values(scope);
    values.push(number.to_string().into());
    values.push(except.unwrap_or(Uuid::nil()).into());
    let clash = db
        .query_one(stmt(
            "SELECT 1 FROM enterprise_extensions WHERE tenant_id = $1 AND application_id = $2 \
             AND environment_id = $3 AND status = 'ACTIVE' AND id <> $5 AND (extension_number = $4 \
             OR ($4 ~ '^[0-9]+$' AND extension_number ~ '^[0-9]+$' \
                 AND (starts_with(extension_number, $4) OR starts_with($4, extension_number)))) \
             LIMIT 1",
            values,
        ))
        .await?;
    if clash.is_some() {
        return Err(EnterpriseError::Conflict(
            "the extension number is in use or ambiguous with another extension",
        ));
    }
    Ok(())
}

fn label(value: &str) -> EnterpriseResult<String> {
    clean_name(value, 100, "displayLabel must be 1-100 characters.")
}

impl EnterpriseService {
    pub async fn create_extension(
        &self,
        actor: &AdminActor,
        new: NewExtension,
    ) -> EnterpriseResult<ExtensionView> {
        let display_label = label(&new.display_label)?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let governing = self
            .governing_policy(&txn, actor.scope, new.policy_id)
            .await?;
        let number = normalize_number(&new.extension_number, &governing)?;
        destinations::validate(
            &txn,
            actor.scope,
            new.destination,
            destinations::EXTENSION_TARGETS,
        )
        .await?;
        lock_namespace(&txn, actor.scope).await?;
        check_namespace(&txn, actor.scope, &number, None).await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            number.clone().into(),
            display_label.into(),
            new.destination.kind.as_str().into(),
            new.destination.id.into(),
            new.policy_id.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_extensions (id, tenant_id, application_id, environment_id, \
             extension_number, display_label, destination_type, destination_id, policy_id, status, \
             version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'ACTIVE', 1, $10, $10)",
            values,
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Extension,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("destination_type", new.destination.kind.as_str())
                    .with("named_policy", new.policy_id.is_some()),
            },
            now,
        )
        .await?;
        let row = extension_row(&txn, actor.scope, id, false).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "EXTENSION"), ("operation", "CREATE")],
        );
        row.try_into()
    }

    /// The rules the extension's number format follows: its named ACTIVE policy (a named
    /// policy that is not ACTIVE in this scope is refused), else the baseline.
    async fn governing_policy<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: CallScope,
        policy_id: Option<Uuid>,
    ) -> EnterpriseResult<PolicyRules> {
        if let Some(id) = policy_id {
            let row = policy::policy_row(db, scope, id, false).await?;
            if row.status != "ACTIVE" {
                return Err(EnterpriseError::Conflict("the named policy is disabled"));
            }
            return row.rules();
        }
        Ok(policy::effective_policy(db, scope, None).await?.rules)
    }

    pub async fn get_extension(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<ExtensionView> {
        extension_row(self.db(), scope, id, false).await?.try_into()
    }

    pub async fn list_extensions(
        &self,
        scope: CallScope,
        filter: &ExtensionFilter,
        page: &PageRequest,
    ) -> EnterpriseResult<ExtensionPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_extensions \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = filter.status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some(kind) = filter.destination_type {
            values.push(kind.as_str().into());
            sql.push_str(&format!(" AND destination_type = ${}", values.len()));
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
        let mut rows = ExtensionRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        Ok(ExtensionPage {
            extensions: rows
                .into_iter()
                .map(ExtensionView::try_from)
                .collect::<EnterpriseResult<_>>()?,
            next_cursor,
        })
    }

    pub async fn update_extension(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        change: ExtensionChange,
    ) -> EnterpriseResult<ExtensionView> {
        let display_label = change.display_label.as_deref().map(label).transpose()?;
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        lock_namespace(&txn, actor.scope).await?;
        let row = extension_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "a disabled extension cannot be changed",
            ));
        }
        let policy_id = change.policy_id.unwrap_or(row.policy_id);
        let governing = self.governing_policy(&txn, actor.scope, policy_id).await?;
        let number = match &change.extension_number {
            Some(n) => normalize_number(n, &governing)?,
            None => row.extension_number.clone(),
        };
        if number != row.extension_number {
            check_namespace(&txn, actor.scope, &number, Some(id)).await?;
        }
        let destination = match change.destination {
            Some(dest) => {
                destinations::validate(&txn, actor.scope, dest, destinations::EXTENSION_TARGETS)
                    .await?;
                dest
            }
            None => row.destination()?,
        };
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_extensions SET extension_number = $2, display_label = $3, \
             destination_type = $4, destination_id = $5, policy_id = $6, version = version + 1, \
             updated_at = GREATEST(updated_at, $7) WHERE id = $1",
            vec![
                id.into(),
                number.clone().into(),
                display_label.unwrap_or(row.display_label.clone()).into(),
                destination.kind.as_str().into(),
                destination.id.into(),
                policy_id.into(),
                now.into(),
            ],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Extension,
                resource_id: id,
                operation: AuditOperation::Update,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("destination_type", destination.kind.as_str())
                    .with("renumbered", number != row.extension_number)
                    .with("destination_changed", destination != row.destination()?),
            },
            now,
        )
        .await?;
        let row = extension_row(&txn, actor.scope, id, false).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "EXTENSION"), ("operation", "UPDATE")],
        );
        row.try_into()
    }

    /// Offboarding (§30): disables future routing; never deletes the extension or any
    /// Call evidence. Restrictive: never blocked by a kill switch or a capability.
    pub async fn disable_extension(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<ExtensionView> {
        let txn = self.begin().await?;
        let row = extension_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the extension is already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_extensions SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::Extension,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let row = extension_row(&txn, actor.scope, id, false).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "EXTENSION"), ("operation", "DISABLE")],
        );
        row.try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(format: ExtensionFormat, min: u8, max: u8) -> PolicyRules {
        PolicyRules {
            extension_format: format,
            extension_min_length: min,
            extension_max_length: max,
            ..PolicyRules::builtin_default()
        }
    }

    #[test]
    fn numeric_numbers_follow_the_policy() {
        let r = rules(ExtensionFormat::Numeric, 3, 4);
        assert_eq!(normalize_number(" 1001 ", &r).unwrap(), "1001");
        for bad in ["10", "10010", "10a1", "+101", "", "１０１"] {
            assert!(normalize_number(bad, &r).is_err(), "{bad}");
        }
    }

    #[test]
    fn alphanumeric_numbers_are_lower_case_words() {
        let r = rules(ExtensionFormat::Alphanumeric, 2, 32);
        assert_eq!(normalize_number("Sales", &r).unwrap(), "sales");
        assert_eq!(normalize_number("support-2", &r).unwrap(), "support-2");
        for bad in [
            "2sales",
            "sa les",
            "sales_team",
            "tel:123",
            "x1234567",
            "-sales",
        ] {
            assert!(normalize_number(bad, &r).is_err(), "{bad}");
        }
    }
}
