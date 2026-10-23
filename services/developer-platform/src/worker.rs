//! Worker foundation: the Developer Platform's own background loop (no generic
//! mega-worker). Each pass records the instance's heartbeat; Section 50+ jobs plug into
//! [`run_once`].

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};

/// Records one heartbeat of `instance` (first beat inserts, later beats advance).
pub async fn heartbeat_once<C: ConnectionTrait>(
    db: &C,
    instance: &str,
    runtime_environment: &str,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO devplat_worker_heartbeats \
             (instance_id, runtime_environment, started_at, last_seen_at, beats) \
         VALUES ($1, $2, $3, $3, 1) \
         ON CONFLICT (instance_id) DO UPDATE \
             SET last_seen_at = GREATEST(devplat_worker_heartbeats.last_seen_at, EXCLUDED.last_seen_at), \
                 beats = devplat_worker_heartbeats.beats + 1",
        vec![instance.into(), runtime_environment.into(), now.into()],
    ))
    .await?;
    Ok(())
}

/// One worker pass: heartbeat, then the Section 50 sweeps (expired invitations).
pub async fn run_once(
    db: &DatabaseConnection,
    instance: &str,
    runtime_environment: &str,
) -> Result<(), DbErr> {
    heartbeat_once(db, instance, runtime_environment, Utc::now()).await?;
    let expired = crate::control::members::expire_due(db).await?;
    if expired > 0 {
        tracing::info!(expired, "pending invitations expired");
    }
    Ok(())
}

/// Runs passes every `interval` until `shutdown` resolves. A failed pass is logged and
/// retried on the next tick (the loop never exits on a transient error).
pub async fn run(
    db: DatabaseConnection,
    instance: String,
    runtime_environment: String,
    interval: Duration,
    shutdown: impl std::future::Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    loop {
        if let Err(e) = run_once(&db, &instance, &runtime_environment).await {
            tracing::warn!(summary = %crate::db::summarize_db_error(&e), "worker pass failed");
        }
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}
