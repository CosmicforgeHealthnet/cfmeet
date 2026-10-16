//! The identity change feed (contract `cfmeet.platform-access.identity` `ChangePage`).
//!
//! Consumers read pages after a cursor (`after` = the last sequence they applied) and
//! apply each change's snapshot when its `revision` is newer than the one they hold.
//! Sequences are monotonic in commit order (see migration 000002), so a consumer that
//! persists its cursor after applying a page never skips a change and can replay a page
//! safely (applying a snapshot is idempotent).

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    snapshots::{
        camel_object, ApplicationScopeSnapshot, ApplicationSnapshot, CredentialScopeSnapshot,
        CredentialSnapshot, EnvironmentSnapshot, PrincipalSnapshot, ServiceAccountSnapshot,
        TenantSnapshot,
    },
    PlatformError, PlatformResult,
};

/// Largest page a consumer may request.
pub const MAX_PAGE: u64 = 500;

text_enum!(
    /// Kind of identity entity a change describes.
    EntityKind {
        Tenant = "TENANT",
        Application = "APPLICATION",
        Environment = "ENVIRONMENT",
        ServiceAccount = "SERVICE_ACCOUNT",
        ApplicationScope = "APPLICATION_SCOPE",
        Credential = "CREDENTIAL",
        CredentialScope = "CREDENTIAL_SCOPE",
        Principal = "PRINCIPAL",
    }
);

text_enum!(ChangeOperation {
    Insert = "INSERT",
    Update = "UPDATE",
    Delete = "DELETE",
});

/// One recorded change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityChange {
    pub sequence: i64,
    pub entity_kind: EntityKind,
    pub operation: ChangeOperation,
    pub entity_key: String,
    pub revision: Option<i64>,
    pub occurred_at: DateTime<Utc>,
    /// The typed snapshot of `entity_kind` (camelCase, never verifier material).
    pub snapshot: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePage {
    pub changes: Vec<IdentityChange>,
    /// The cursor to pass as `after` next (the last returned sequence, or `after`).
    pub next_after: i64,
    pub has_more: bool,
}

#[derive(FromQueryResult)]
struct ChangeRow {
    sequence: i64,
    entity_kind: String,
    operation: String,
    entity_key: String,
    revision: Option<i64>,
    occurred_at: DateTime<Utc>,
    snapshot: Value,
}

/// Validates a stored snapshot against its typed contract shape and returns it as wire
/// JSON. A snapshot that does not match is an internal error, never passed through.
fn typed_snapshot(kind: EntityKind, row: Value) -> PlatformResult<Value> {
    fn check<T: serde::de::DeserializeOwned + Serialize>(v: Value) -> PlatformResult<Value> {
        let typed: T = serde_json::from_value(v).map_err(|_| PlatformError::Database {
            summary: "identity change snapshot does not match its contract".to_string(),
            connectivity: false,
        })?;
        serde_json::to_value(typed).map_err(|_| PlatformError::Database {
            summary: "identity change snapshot cannot be serialized".to_string(),
            connectivity: false,
        })
    }
    let v = camel_object(row);
    match kind {
        EntityKind::Tenant => check::<TenantSnapshot>(v),
        EntityKind::Application => check::<ApplicationSnapshot>(v),
        EntityKind::Environment => check::<EnvironmentSnapshot>(v),
        EntityKind::ServiceAccount => check::<ServiceAccountSnapshot>(v),
        EntityKind::ApplicationScope => check::<ApplicationScopeSnapshot>(v),
        EntityKind::Credential => check::<CredentialSnapshot>(v),
        EntityKind::CredentialScope => check::<CredentialScopeSnapshot>(v),
        EntityKind::Principal => check::<PrincipalSnapshot>(v),
    }
}

/// Reads at most `limit` changes with `sequence > after`.
pub async fn read_page<C: ConnectionTrait>(db: &C, after: i64, limit: u64) -> PlatformResult<ChangePage> {
    let limit = limit.clamp(1, MAX_PAGE);
    let rows = ChangeRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT sequence, entity_kind, operation, entity_key, revision, occurred_at, snapshot \
         FROM platform_identity_changes WHERE sequence > $1 ORDER BY sequence LIMIT $2",
        [after.max(0).into(), (limit as i64 + 1).into()],
    ))
    .all(db)
    .await?;
    let has_more = rows.len() as u64 > limit;
    let mut changes = Vec::with_capacity(rows.len().min(limit as usize));
    for row in rows.into_iter().take(limit as usize) {
        let kind = EntityKind::parse(&row.entity_kind).ok_or_else(|| PlatformError::Database {
            summary: "unknown identity change kind".to_string(),
            connectivity: false,
        })?;
        let operation =
            ChangeOperation::parse(&row.operation).ok_or_else(|| PlatformError::Database {
                summary: "unknown identity change operation".to_string(),
                connectivity: false,
            })?;
        changes.push(IdentityChange {
            sequence: row.sequence,
            entity_kind: kind,
            operation,
            entity_key: row.entity_key,
            revision: row.revision,
            occurred_at: row.occurred_at,
            snapshot: typed_snapshot(kind, row.snapshot)?,
        });
    }
    let next_after = changes.last().map(|c| c.sequence).unwrap_or(after.max(0));
    Ok(ChangePage {
        changes,
        next_after,
        has_more,
    })
}
