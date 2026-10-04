//! Outbox claiming with crash-safe leases (Part E §11, §13).
//!
//! A claim moves `available_at` into the future (the lease) and records the holder
//! in one statement guarded by `FOR UPDATE SKIP LOCKED`: concurrent relays never claim
//! the same row, and a relay that crashes mid-delivery simply lets its lease expire —
//! the row becomes claimable again. No delivery work happens inside a transaction.

use chrono::{Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

use crate::{
    domain::{OutboxChannel, OutboxStatus},
    error::ApiResult,
    models::outbox_events,
};

/// Delivery attempts after which a non-transient failure is dead-lettered (FAILED).
pub const MAX_ATTEMPTS: i32 = 10;
/// Upper bound for retry backoff.
pub const MAX_BACKOFF_SECS: i64 = 60;

/// Claim up to `batch` due PENDING rows of `channel` (optionally one conference).
pub async fn claim(
    db: &DatabaseConnection,
    channel: OutboxChannel,
    conference_id: Option<Uuid>,
    batch: u64,
    lease_secs: i64,
    holder: &str,
) -> ApiResult<Vec<outbox_events::Model>> {
    let (filter, values) = match conference_id {
        Some(cid) => (
            "AND conference_id = $5",
            vec![
                channel.as_str().into(),
                (batch as i64).into(),
                lease_secs.into(),
                holder.into(),
                cid.into(),
            ],
        ),
        None => (
            "",
            vec![
                channel.as_str().into(),
                (batch as i64).into(),
                lease_secs.into(),
                holder.into(),
            ],
        ),
    };
    let sql = format!(
        r#"UPDATE outbox_events o
              SET available_at = now() + make_interval(secs => $3),
                  attempts = o.attempts + 1,
                  locked_by = $4
            WHERE o.id IN (
                SELECT id FROM outbox_events
                 WHERE status = 'PENDING' AND channel = $1 AND available_at <= now() {filter}
                 ORDER BY conference_id NULLS FIRST, conference_seq NULLS FIRST, created_at
                 LIMIT $2
                 FOR UPDATE SKIP LOCKED)
        RETURNING o.*"#
    );
    let mut rows = outbox_events::Model::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .all(db)
    .await?;
    rows.sort_by_key(|r| (r.conference_seq, r.created_at));
    Ok(rows)
}

pub async fn mark_published(db: &DatabaseConnection, id: Uuid, holder: &str) -> ApiResult<()> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE outbox_events SET status = 'PUBLISHED', published_at = now(), last_error = NULL, locked_by = NULL \
          WHERE id = $1 AND status = 'PENDING' AND locked_by = $2",
        [id.into(), holder.into()],
    ))
    .await?;
    Ok(())
}

/// Release the lease for a retry. Transient failures (provider/Redis outage) retry
/// indefinitely with capped backoff; other failures are dead-lettered after
/// [`MAX_ATTEMPTS`]. Returns the resulting status.
pub async fn mark_retry(
    db: &DatabaseConnection,
    event: &outbox_events::Model,
    holder: &str,
    error: &str,
    transient: bool,
) -> ApiResult<OutboxStatus> {
    let attempts = event.attempts;
    let error: String = error.chars().take(500).collect();
    let status = if !transient && attempts >= MAX_ATTEMPTS {
        OutboxStatus::Failed
    } else {
        OutboxStatus::Pending
    };
    let backoff = 2i64.pow(attempts.clamp(0, 6) as u32).min(MAX_BACKOFF_SECS);
    let available_at = Utc::now() + Duration::seconds(backoff);
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE outbox_events SET status = $3, available_at = $4, last_error = $5, locked_by = NULL \
          WHERE id = $1 AND status = 'PENDING' AND locked_by = $2",
        [
            event.id.into(),
            holder.into(),
            status.as_str().into(),
            available_at.into(),
            error.into(),
        ],
    ))
    .await?;
    Ok(status)
}

/// Dead-letter immediately (poison event: malformed / unknown / permanently invalid).
pub async fn mark_failed(
    db: &DatabaseConnection,
    id: Uuid,
    holder: &str,
    error: &str,
) -> ApiResult<()> {
    let error: String = error.chars().take(500).collect();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE outbox_events SET status = 'FAILED', last_error = $3, locked_by = NULL \
          WHERE id = $1 AND status = 'PENDING' AND locked_by = $2",
        [id.into(), holder.into(), error.into()],
    ))
    .await?;
    Ok(())
}

/// Unique lease-holder id for one relay/dispatcher instance.
pub fn holder_id(kind: &str) -> String {
    format!("{kind}:{}:{}", std::process::id(), Uuid::new_v4().simple())
}

/// Pending counts per channel (observability).
pub async fn backlog(db: &DatabaseConnection) -> ApiResult<(i64, i64)> {
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) FILTER (WHERE channel = 'REALTIME'), count(*) FILTER (WHERE channel = 'PROVIDER_COMMAND') \
               FROM outbox_events WHERE status = 'PENDING'"
                .to_string(),
        ))
        .await?;
    Ok(row
        .map(|r| {
            (
                r.try_get_by_index::<i64>(0).unwrap_or(0),
                r.try_get_by_index::<i64>(1).unwrap_or(0),
            )
        })
        .unwrap_or((0, 0)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn backoff_is_capped() {
        for attempts in 0..20i32 {
            let b = 2i64
                .pow(attempts.clamp(0, 6) as u32)
                .min(super::MAX_BACKOFF_SECS);
            assert!(b <= super::MAX_BACKOFF_SECS);
        }
    }
}
