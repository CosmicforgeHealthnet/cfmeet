//! Append-only audit of key and device changes (`messaging_audit`; the database refuses
//! updates, deletes and secret-looking detail keys).

use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use serde_json::Value;
use uuid::Uuid;

use crate::{db, domain::RequestContext, error::Result, identity::TrustedContext};

pub struct AuditEntry<'a> {
    pub ctx: &'a TrustedContext,
    pub principal_id: Option<Uuid>,
    pub action: &'static str,
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub detail: Value,
}

pub async fn record<C: ConnectionTrait>(
    db: &C,
    entry: AuditEntry<'_>,
    request: &RequestContext,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "INSERT INTO messaging_audit (id, occurred_at, tenant_id, application_id, environment_id, \
           credential_id, principal_id, action, target_kind, target_id, correlation_id, detail) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        vec![
            Uuid::new_v4().into(),
            now.into(),
            entry.ctx.scope.tenant_id.into(),
            entry.ctx.scope.application_id.into(),
            entry.ctx.scope.environment_id.into(),
            entry.ctx.credential_id.into(),
            entry.principal_id.into(),
            entry.action.into(),
            entry.target_kind.into(),
            entry.target_id.into(),
            request.correlation_id.as_str().into(),
            entry.detail.into(),
        ],
    )
    .await?;
    Ok(())
}
