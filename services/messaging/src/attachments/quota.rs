//! Attachment quotas (§78), settled atomically in the database (never from client
//! totals). Per environment: committed + reserved bytes against the storage budget (the
//! application policy's, bounded by the deployment's `MESSAGING_STORAGE_BUDGET_BYTES`),
//! the daily upload volume, and per uploader the number of concurrent uploads. The
//! usage row is locked for the decision, so concurrent intents cannot overshoot.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use uuid::Uuid;

use super::AttachmentRow;
use crate::{
    conversations::policy::AttachmentPolicy,
    db,
    error::{Error, ErrorCode, Result},
    identity::TrustedContext,
    state::AppState,
};

#[derive(FromQueryResult)]
struct Usage {
    committed_bytes: i64,
    reserved_bytes: i64,
}

/// The environment's storage budget: the application policy's, bounded by the
/// deployment's `MESSAGING_STORAGE_BUDGET_BYTES` (`None`: unlimited).
pub fn effective_budget(policy: &AttachmentPolicy, deployment: Option<i64>) -> Option<i64> {
    match (policy.storage_budget_bytes, deployment) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Reserves `size` bytes for a new upload intent of `uploader`.
pub async fn reserve<C: ConnectionTrait>(
    db: &C,
    state: &AppState,
    ctx: &TrustedContext,
    uploader: Uuid,
    size: i64,
    policy: &AttachmentPolicy,
    now: DateTime<Utc>,
) -> Result<()> {
    let env = ctx.scope.environment_id;
    db::exec(
        db,
        "INSERT INTO messaging_storage_usage (environment_id, tenant_id, application_id, updated_at) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (environment_id) DO NOTHING",
        vec![env.into(), ctx.scope.tenant_id.into(), ctx.scope.application_id.into(), now.into()],
    )
    .await?;
    let usage: Usage = db::one(
        db,
        "SELECT committed_bytes, reserved_bytes FROM messaging_storage_usage WHERE environment_id = $1 FOR UPDATE",
        vec![env.into()],
    )
    .await?
    .ok_or(Error::code(ErrorCode::InternalError))?;
    let budget = effective_budget(policy, state.attachments.storage_budget_bytes);
    if budget.is_some_and(|b| usage.committed_bytes + usage.reserved_bytes + size > b) {
        crate::metrics::inc(
            "messaging_attachment_quota_refusals_total",
            &[("quota", "storage")],
        );
        return Err(Error::reason(ErrorCode::QuotaExceeded, "STORAGE_BUDGET"));
    }
    let concurrent = db::scalar_i64(
        db,
        "SELECT count(*) FROM messaging_attachments WHERE environment_id = $1 AND uploader_principal_id = $2 \
           AND state = 'INTENT_CREATED' AND intent_expires_at > $3",
        vec![env.into(), uploader.into(), now.into()],
    )
    .await?;
    if concurrent >= policy.max_concurrent_uploads {
        crate::metrics::inc(
            "messaging_attachment_quota_refusals_total",
            &[("quota", "concurrent")],
        );
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "CONCURRENT_UPLOADS",
        ));
    }
    let today = now.date_naive();
    let daily = db::scalar_i64(
        db,
        "INSERT INTO messaging_upload_days (environment_id, day, uploaded_bytes) VALUES ($1, $2, $3) \
         ON CONFLICT (environment_id, day) DO UPDATE SET uploaded_bytes = messaging_upload_days.uploaded_bytes + EXCLUDED.uploaded_bytes \
         RETURNING uploaded_bytes",
        vec![env.into(), today.into(), size.into()],
    )
    .await?;
    if policy.daily_upload_bytes.is_some_and(|d| daily > d) {
        crate::metrics::inc(
            "messaging_attachment_quota_refusals_total",
            &[("quota", "daily")],
        );
        // The transaction rolls back: the daily counter is not consumed by a refusal.
        return Err(Error::reason(
            ErrorCode::QuotaExceeded,
            "DAILY_UPLOAD_VOLUME",
        ));
    }
    db::exec(
        db,
        "UPDATE messaging_storage_usage SET reserved_bytes = reserved_bytes + $2, updated_at = $3 WHERE environment_id = $1",
        vec![env.into(), size.into(), now.into()],
    )
    .await?;
    Ok(())
}

/// The reservation of a stored object becomes committed usage.
pub async fn commit<C: ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "UPDATE messaging_attachments SET committed_bytes = reserved_bytes, reserved_bytes = 0 WHERE id = $1 AND reserved_bytes > 0",
        vec![row.id.into()],
    )
    .await?;
    db::exec(
        db,
        "UPDATE messaging_storage_usage SET reserved_bytes = GREATEST(reserved_bytes - $2, 0), committed_bytes = committed_bytes + $2, \
           updated_at = $3 WHERE environment_id = $1",
        vec![row.environment_id.into(), row.reserved_bytes.into(), now.into()],
    )
    .await?;
    Ok(())
}

/// A failed / expired upload gives its storage reservation back. The daily upload volume
/// is NOT refunded: it bounds upload traffic, and abandoned uploads still consumed it.
pub async fn release<C: ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    now: DateTime<Utc>,
) -> Result<()> {
    if row.reserved_bytes == 0 {
        return Ok(());
    }
    db::exec(
        db,
        "UPDATE messaging_attachments SET reserved_bytes = 0 WHERE id = $1",
        vec![row.id.into()],
    )
    .await?;
    db::exec(
        db,
        "UPDATE messaging_storage_usage SET reserved_bytes = GREATEST(reserved_bytes - $2, 0), updated_at = $3 WHERE environment_id = $1",
        vec![row.environment_id.into(), row.reserved_bytes.into(), now.into()],
    )
    .await?;
    Ok(())
}

/// A deleted attachment gives up its committed bytes and reservation; returns the
/// committed bytes (read from the locked row, never from a possibly stale copy) so the
/// caller can move them to the object's deletion request.
pub async fn free<C: ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    now: DateTime<Utc>,
) -> Result<i64> {
    let Some(old) = db::one::<Usage, _>(
        db,
        "WITH old AS (SELECT id, committed_bytes, reserved_bytes FROM messaging_attachments WHERE id = $1 FOR UPDATE) \
         UPDATE messaging_attachments a SET committed_bytes = 0, reserved_bytes = 0 FROM old WHERE a.id = old.id \
         RETURNING old.committed_bytes, old.reserved_bytes",
        vec![row.id.into()],
    )
    .await?
    else {
        return Ok(0);
    };
    db::exec(
        db,
        "UPDATE messaging_storage_usage SET committed_bytes = GREATEST(committed_bytes - $2, 0), \
           reserved_bytes = GREATEST(reserved_bytes - $3, 0), updated_at = $4 WHERE environment_id = $1",
        vec![row.environment_id.into(), old.committed_bytes.into(), old.reserved_bytes.into(), now.into()],
    )
    .await?;
    Ok(old.committed_bytes)
}

/// Recorded and recomputed usage of one environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromQueryResult)]
pub struct UsageAudit {
    pub environment_id: Uuid,
    pub recorded_committed: i64,
    pub expected_committed: i64,
    pub recorded_reserved: i64,
    pub expected_reserved: i64,
}

impl UsageAudit {
    pub fn drifted(&self) -> bool {
        self.recorded_committed != self.expected_committed
            || self.recorded_reserved != self.expected_reserved
    }
}

const AUDIT_SQL: &str = "SELECT u.environment_id, u.committed_bytes AS recorded_committed, u.reserved_bytes AS recorded_reserved, \
    (COALESCE((SELECT sum(a.committed_bytes) FROM messaging_attachments a WHERE a.environment_id = u.environment_id), 0) \
     + COALESCE((SELECT sum(d.bytes) FROM messaging_attachment_derivatives d WHERE d.environment_id = u.environment_id \
                   AND d.kind = 'THUMBNAIL' AND d.status = 'READY'), 0) \
     + COALESCE((SELECT sum(o.charged_bytes) FROM messaging_object_deletions o WHERE o.environment_id = u.environment_id \
                   AND o.status IN ('PENDING', 'HELD', 'DEAD')), 0))::bigint AS expected_committed, \
    (COALESCE((SELECT sum(a.reserved_bytes) FROM messaging_attachments a WHERE a.environment_id = u.environment_id), 0) \
     + COALESCE((SELECT sum(d.reserved_bytes) FROM messaging_attachment_derivatives d WHERE d.environment_id = u.environment_id), 0) \
    )::bigint AS expected_reserved \
    FROM messaging_storage_usage u";

/// Recomputes one environment's usage from the rows it is made of (read-only).
pub async fn audit<C: ConnectionTrait>(db: &C, environment_id: Uuid) -> Result<Option<UsageAudit>> {
    Ok(db::one(
        db,
        &format!("{AUDIT_SQL} WHERE u.environment_id = $1"),
        vec![environment_id.into()],
    )
    .await?)
}

/// Every environment whose recorded usage differs from its rows (read-only).
pub async fn audit_all<C: ConnectionTrait>(db: &C) -> Result<Vec<UsageAudit>> {
    let all: Vec<UsageAudit> = db::all(db, AUDIT_SQL, vec![]).await?;
    let drifted: Vec<UsageAudit> = all.into_iter().filter(UsageAudit::drifted).collect();
    for d in &drifted {
        tracing::error!(
            environment = %d.environment_id,
            committed_drift = d.recorded_committed - d.expected_committed,
            reserved_drift = d.recorded_reserved - d.expected_reserved,
            "messaging storage accounting drift"
        );
    }
    Ok(drifted)
}

/// Repairs one environment's recorded usage under its row lock (an operator action,
/// e.g. after restoring a backup). Returns the audit taken under the lock, before repair.
pub async fn reconcile<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<UsageAudit>> {
    db::exec(
        db,
        "SELECT 1 FROM messaging_storage_usage WHERE environment_id = $1 FOR UPDATE",
        vec![environment_id.into()],
    )
    .await?;
    let Some(a) = audit(db, environment_id).await? else {
        return Ok(None);
    };
    if a.drifted() {
        db::exec(
            db,
            "UPDATE messaging_storage_usage SET committed_bytes = $2, reserved_bytes = $3, updated_at = $4 WHERE environment_id = $1",
            vec![environment_id.into(), a.expected_committed.into(), a.expected_reserved.into(), now.into()],
        )
        .await?;
        crate::metrics::inc("messaging_storage_accounting_repairs_total", &[]);
    }
    Ok(Some(a))
}
