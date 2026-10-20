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
    let budget = match (
        policy.storage_budget_bytes,
        state.attachments.storage_budget_bytes,
    ) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
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

/// A deleted object frees its committed bytes.
pub async fn free<C: ConnectionTrait>(
    db: &C,
    row: &AttachmentRow,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "UPDATE messaging_attachments SET committed_bytes = 0, reserved_bytes = 0 WHERE id = $1",
        vec![row.id.into()],
    )
    .await?;
    db::exec(
        db,
        "UPDATE messaging_storage_usage SET committed_bytes = GREATEST(committed_bytes - $2, 0), \
           reserved_bytes = GREATEST(reserved_bytes - $3, 0), updated_at = $4 WHERE environment_id = $1",
        vec![row.environment_id.into(), row.committed_bytes.into(), row.reserved_bytes.into(), now.into()],
    )
    .await?;
    Ok(())
}
