//! Worker foundation: the Developer Platform's own background loop (no generic
//! mega-worker). Each pass records the instance's heartbeat; Section 50+ jobs plug into
//! [`run_once_with`] (Section 53: the central webhook delivery pass; Section 15: the
//! Platform Access reconciliation of organization creations and renames).

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};

use crate::{
    control::credential_lifecycle::{self, CredentialLifecycleSettings, SweepReport},
    platform_access::PlatformAccessPort,
    reference_reconciliation::{self, ReferenceReport},
    webhooks::WebhookService,
};

/// The worker's periodic jobs: each runs at most once per interval across every worker
/// instance (`devplat_worker_schedules`). [`Default`]: none (the foundation loop only).
#[derive(Debug, Clone)]
pub struct PeriodicJobs {
    /// Section 15 (PLT-008): credential lifecycle webhooks.
    pub credential_lifecycle: CredentialLifecycleSettings,
    /// DATA-017: report-only reference reconciliation (None: off).
    pub reference_reconciliation_interval: Option<Duration>,
    /// DATA-017: ids of each kind verified at Platform Access per report.
    pub reference_reconciliation_remote_limit: u64,
}

impl Default for PeriodicJobs {
    fn default() -> Self {
        Self {
            credential_lifecycle: CredentialLifecycleSettings::disabled(),
            reference_reconciliation_interval: None,
            reference_reconciliation_remote_limit: 0,
        }
    }
}

/// Claims the run of `job` at `now` if its last start is at least `interval` ago (or it
/// never ran). Exactly one of several concurrent claimers wins.
pub async fn claim_schedule<C: ConnectionTrait>(
    db: &C,
    job: &str,
    interval: Duration,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let claimed = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO devplat_worker_schedules (job, last_started_at) VALUES ($1, $2) \
             ON CONFLICT (job) DO UPDATE \
                 SET last_started_at = EXCLUDED.last_started_at, last_completed_at = NULL, last_outcome = NULL \
               WHERE devplat_worker_schedules.last_started_at <= EXCLUDED.last_started_at - make_interval(secs => $3)",
            vec![job.into(), now.into(), interval.as_secs_f64().into()],
        ))
        .await?
        .rows_affected();
    Ok(claimed == 1)
}

/// Records the outcome (`SUCCEEDED` / `FAILED` / `ATTENTION`) of the claimed run of `job`.
pub async fn complete_schedule<C: ConnectionTrait>(
    db: &C,
    job: &str,
    outcome: &str,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE devplat_worker_schedules SET last_completed_at = $2, last_outcome = $3 WHERE job = $1",
        vec![job.into(), now.into(), outcome.into()],
    ))
    .await?;
    Ok(())
}

/// Section 15 (PLT-008): one credential lifecycle sweep when its interval has elapsed.
/// `None`: not due, disabled, or failed (logged; the next due pass retries).
pub async fn credential_lifecycle_pass(
    webhooks: &WebhookService,
    platform: &dyn PlatformAccessPort,
    settings: &CredentialLifecycleSettings,
) -> Option<SweepReport> {
    if !settings.enabled || !webhooks.settings().enabled {
        return None;
    }
    let now = webhooks.now();
    let db = webhooks.db();
    match claim_schedule(db, credential_lifecycle::JOB, settings.interval, now).await {
        Ok(true) => {}
        Ok(false) => return None,
        Err(e) => {
            tracing::warn!(summary = %crate::db::summarize_db_error(&e), "lifecycle sweep not claimed");
            return None;
        }
    }
    let result = credential_lifecycle::sweep_once(webhooks, platform, settings, now).await;
    let outcome = match &result {
        Ok(r) if r.unavailable > 0 || r.inconsistent > 0 => "ATTENTION",
        Ok(_) => "SUCCEEDED",
        Err(_) => "FAILED",
    };
    if let Err(e) = complete_schedule(db, credential_lifecycle::JOB, outcome, webhooks.now()).await
    {
        tracing::warn!(summary = %crate::db::summarize_db_error(&e), "lifecycle sweep outcome not recorded");
    }
    match result {
        Ok(r) => {
            tracing::info!(
                environments = r.environments,
                unavailable = r.unavailable,
                examined = r.examined,
                notified = r.notified,
                deliveries = r.deliveries,
                deduplicated = r.deduplicated,
                inconsistent = r.inconsistent,
                "credential lifecycle sweep"
            );
            Some(r)
        }
        Err(e) => {
            tracing::warn!(code = e.0.as_str(), "credential lifecycle sweep failed");
            None
        }
    }
}

/// DATA-017: the report-only reference reconciliation when its interval has elapsed
/// (logged; nothing is repaired). `None`: not due, off, or failed.
pub async fn reference_reconciliation_pass(
    db: &DatabaseConnection,
    platform: Option<&dyn PlatformAccessPort>,
    interval: Duration,
    remote_limit: u64,
) -> Option<ReferenceReport> {
    let now = Utc::now();
    match claim_schedule(db, reference_reconciliation::JOB, interval, now).await {
        Ok(true) => {}
        Ok(false) => return None,
        Err(e) => {
            tracing::warn!(summary = %crate::db::summarize_db_error(&e), "reference reconciliation not claimed");
            return None;
        }
    }
    let platform = platform.filter(|_| remote_limit > 0);
    let result = reference_reconciliation::full_report(db, platform, remote_limit).await;
    let outcome = match &result {
        Ok(r) if r.anomalies > 0 => "ATTENTION",
        Ok(_) => "SUCCEEDED",
        Err(_) => "FAILED",
    };
    if let Err(e) = complete_schedule(db, reference_reconciliation::JOB, outcome, Utc::now()).await
    {
        tracing::warn!(summary = %crate::db::summarize_db_error(&e), "reference reconciliation outcome not recorded");
    }
    match result {
        Ok(r) => {
            for c in r.checks.iter().filter(|c| c.anomalies > 0) {
                tracing::warn!(
                    check = c.name,
                    owner = c.owner,
                    rows = c.rows,
                    anomalies = c.anomalies,
                    "reference reconciliation anomaly (report only)"
                );
            }
            if let Some(remote) = &r.remote {
                for f in &remote.findings {
                    tracing::warn!(kind = f.kind, id = %f.id, problem = f.problem,
                        "reference missing or mismatched at Platform Access (report only)");
                }
            }
            tracing::info!(
                checks = r.checks.len(),
                anomalies = r.anomalies,
                remote = r.remote_verification,
                "reference reconciliation"
            );
            Some(r)
        }
        Err(e) => {
            tracing::warn!(summary = %crate::db::summarize_db_error(&e), "reference reconciliation failed");
            None
        }
    }
}

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
    run_pass(db, instance, runtime_environment, webhooks, None).await
}

/// Section 15 (ORG-014 / ORG-013): settles organization creations and renames that did
/// not complete against Platform Access. A failure is logged and retried next pass.
pub async fn reconcile_platform(db: &DatabaseConnection, platform: &dyn PlatformAccessPort) {
    match crate::control::organizations::reconcile_once(db, platform).await {
        Ok(r) if r == Default::default() => {}
        Ok(r) => tracing::info!(
            compensated = r.compensated,
            abandoned = r.abandoned,
            completed = r.completed,
            retried = r.retried,
            names_synced = r.names_synced,
            "platform reconciliation pass"
        ),
        Err(e) => tracing::warn!(error = ?e, "platform reconciliation pass failed"),
    }
}

/// One full worker pass: heartbeat, sweeps, the Platform Access reconciliation when an
/// authority is configured, and the webhook delivery pass when enabled.
pub async fn run_pass(
    db: &DatabaseConnection,
    instance: &str,
    runtime_environment: &str,
    webhooks: Option<&WebhookService>,
    platform: Option<&dyn PlatformAccessPort>,
) -> Result<(), DbErr> {
    run_pass_with_jobs(
        db,
        instance,
        runtime_environment,
        webhooks,
        platform,
        &PeriodicJobs::default(),
    )
    .await
}

/// [`run_pass`] plus the due [`PeriodicJobs`]: the PLT-008 expiry notice sweep (before
/// the delivery pass, so its notices go out in the same pass) and the DATA-017 reference
/// reconciliation.
pub async fn run_pass_with_jobs(
    db: &DatabaseConnection,
    instance: &str,
    runtime_environment: &str,
    webhooks: Option<&WebhookService>,
    platform: Option<&dyn PlatformAccessPort>,
    jobs: &PeriodicJobs,
) -> Result<(), DbErr> {
    heartbeat_once(db, instance, runtime_environment, Utc::now()).await?;
    if let Some(p) = platform {
        reconcile_platform(db, p).await;
    }
    let expired = crate::control::members::expire_due(db).await?;
    if expired > 0 {
        tracing::info!(expired, "pending invitations expired");
    }
    if let Some(interval) = jobs.reference_reconciliation_interval {
        reference_reconciliation_pass(
            db,
            platform,
            interval,
            jobs.reference_reconciliation_remote_limit,
        )
        .await;
    }
    if let (Some(w), Some(p)) = (webhooks, platform) {
        credential_lifecycle_pass(w, p, &jobs.credential_lifecycle).await;
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
                released = r.released,
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
    run_with_platform(
        db,
        instance,
        runtime_environment,
        interval,
        webhooks,
        None,
        shutdown,
    )
    .await
}

/// [`run_with`] plus the Section 15 Platform Access reconciliation.
pub async fn run_with_platform(
    db: DatabaseConnection,
    instance: String,
    runtime_environment: String,
    interval: Duration,
    webhooks: Option<WebhookService>,
    platform: Option<Arc<dyn PlatformAccessPort>>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    run_with_jobs(
        db,
        instance,
        runtime_environment,
        interval,
        webhooks,
        platform,
        PeriodicJobs::default(),
        shutdown,
    )
    .await
}

/// [`run_with_platform`] plus the [`PeriodicJobs`] (the worker binary's loop).
#[allow(clippy::too_many_arguments)]
pub async fn run_with_jobs(
    db: DatabaseConnection,
    instance: String,
    runtime_environment: String,
    interval: Duration,
    webhooks: Option<WebhookService>,
    platform: Option<Arc<dyn PlatformAccessPort>>,
    jobs: PeriodicJobs,
    shutdown: impl std::future::Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    loop {
        if let Err(e) = run_pass_with_jobs(
            &db,
            &instance,
            &runtime_environment,
            webhooks.as_ref(),
            platform.as_deref(),
            &jobs,
        )
        .await
        {
            tracing::warn!(summary = %crate::db::summarize_db_error(&e), "worker pass failed");
        }
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
}
