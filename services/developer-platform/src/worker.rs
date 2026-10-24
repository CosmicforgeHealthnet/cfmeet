//! Worker foundation: the Developer Platform's own background loop (no generic
//! mega-worker). Each pass records the instance's heartbeat; Section 50+ jobs plug into
//! [`run_once_with`] (Section 53: the central webhook delivery pass).

use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};

use crate::webhooks::WebhookService;

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

/// Deliveries handled per webhook pass.
pub const WEBHOOK_BATCH: u64 = 100;

/// One worker pass: heartbeat, then the Section 50 sweeps (expired invitations).
pub async fn run_once(
    db: &DatabaseConnection,
    instance: &str,
    runtime_environment: &str,
) -> Result<(), DbErr> {
    run_once_with(db, instance, runtime_environment, None).await
}

/// [`run_once`] plus, when central webhooks are enabled, one Section 53 delivery pass.
pub async fn run_once_with(
    db: &DatabaseConnection,
    instance: &str,
    runtime_environment: &str,
    webhooks: Option<&WebhookService>,
) -> Result<(), DbErr> {
    heartbeat_once(db, instance, runtime_environment, Utc::now()).await?;
    let expired = crate::control::members::expire_due(db).await?;
    if expired > 0 {
        tracing::info!(expired, "pending invitations expired");
    }
    // Section 55: approvals not activated in time.
    match crate::control::production::expire_due(db).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(expired = n, "production-access approvals expired"),
        Err(e) => tracing::warn!(error = ?e, "production-access expiry sweep failed"),
    }
    if let Some(w) = webhooks.filter(|w| w.settings().enabled) {
        let r = w.deliver_due_once(WEBHOOK_BATCH).await?;
        if r != Default::default() {
            tracing::info!(
                succeeded = r.succeeded,
                retried = r.retried,
                failed = r.failed,
                cancelled = r.cancelled,
                shadowed = r.shadowed,
                stale = r.stale,
                disabled = r.disabled,
                mode = w.settings().mode.as_str(),
                "webhook delivery pass"
            );
        }
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
    run_with(db, instance, runtime_environment, interval, None, shutdown).await
}

/// [`run`] with the central webhook delivery worker.
pub async fn run_with(
    db: DatabaseConnection,
    instance: String,
    runtime_environment: String,
    interval: Duration,
    webhooks: Option<WebhookService>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    loop {
        if let Err(e) = run_once_with(&db, &instance, &runtime_environment, webhooks.as_ref()).await
        {
            tracing::warn!(summary = %crate::db::summarize_db_error(&e), "worker pass failed");
        }
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}
