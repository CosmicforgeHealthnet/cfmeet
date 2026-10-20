//! Administrative audit API (§95): the application's own audit trail (append-only
//! `messaging_audit`: devices and keys, membership, legal holds, reports, moderation,
//! exports, data deletion), tenant-scoped, filterable, newest first. Details never hold
//! content, ciphertext or secrets (a database CHECK refuses such keys).

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, Result},
    identity::TrustedContext,
    state::AppState,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditQuery {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub target_kind: Option<String>,
    #[serde(default)]
    pub target_id: Option<Uuid>,
    /// Entries strictly older than this instant (paging).
    #[serde(default)]
    pub before: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(FromQueryResult)]
struct Row {
    id: Uuid,
    occurred_at: DateTime<Utc>,
    credential_id: Uuid,
    principal_id: Option<Uuid>,
    action: String,
    target_kind: String,
    target_id: Uuid,
    correlation_id: String,
    detail: Value,
}

fn code(v: &str) -> bool {
    (2..=48).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
}

pub async fn list(state: &AppState, ctx: &TrustedContext, q: AuditQuery) -> Result<Value> {
    let limit = q.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return Err(Error::validation("limit must be between 1 and 500"));
    }
    if q.action.as_deref().is_some_and(|a| !code(a))
        || q.target_kind.as_deref().is_some_and(|a| !code(a))
    {
        return Err(Error::validation(
            "action / targetKind are UPPER_SNAKE_CASE codes",
        ));
    }
    let rows: Vec<Row> = db::all(
        &state.db,
        "SELECT id, occurred_at, credential_id, principal_id, action, target_kind, target_id, correlation_id, detail \
           FROM messaging_audit WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 \
            AND ($4::varchar IS NULL OR action = $4) AND ($5::varchar IS NULL OR target_kind = $5) \
            AND ($6::uuid IS NULL OR target_id = $6) AND ($7::timestamptz IS NULL OR occurred_at < $7) \
          ORDER BY occurred_at DESC, id LIMIT $8",
        vec![
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            q.action.into(),
            q.target_kind.into(),
            q.target_id.into(),
            q.before.into(),
            (limit + 1).into(),
        ],
    )
    .await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<&Row> = rows.iter().take(limit as usize).collect();
    Ok(json!({
        "entries": page.iter().map(|r| json!({
            "auditId": r.id,
            "occurredAt": rfc3339(r.occurred_at),
            "credentialId": r.credential_id,
            "principalId": r.principal_id,
            "action": r.action,
            "targetKind": r.target_kind,
            "targetId": r.target_id,
            "correlationId": r.correlation_id,
            "detail": r.detail,
        })).collect::<Vec<_>>(),
        "nextBefore": if more { page.last().map(|r| rfc3339(r.occurred_at)) } else { None },
    }))
}
