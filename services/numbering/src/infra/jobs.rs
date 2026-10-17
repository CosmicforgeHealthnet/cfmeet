//! Generic leased job queue — the execution foundation of every later Numbering worker
//! (provider reservation / release, inventory refresh, expiry sweeps, ...).
//!
//! * [`enqueue`] records a job in the caller's transaction (de-duplicated by
//!   `(kind, dedupe_key)`), so work is scheduled atomically with the change that needs it.
//! * [`JobRunner::run_due`] claims due jobs with a lease, an incremented attempt and a
//!   fresh claim token (`FOR UPDATE SKIP LOCKED`): two instances never run the same claim.
//!   Every completion is guarded by the claim token.
//! * A claim whose lease expired (the worker died) is recovered according to the job's
//!   declared [`RetrySafety`]: IDEMPOTENT work is retried; work that may have reached an
//!   external provider (RECONCILE_ON_UNCERTAINTY) is parked as UNKNOWN_OUTCOME and is
//!   NEVER repeated automatically — reconciliation decides ([`resolve_unknown`]).
//! * Transient failures back off exponentially (bounded); a job that exhausts its attempts
//!   is DEAD_LETTER; a permanent failure is FAILED. Both are final and visible.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement};
use serde_json::Value;
use uuid::Uuid;

use super::{bounded_error, RetryPolicy};
use crate::domain::{RequestContext, SharedClock};

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// Whether a job may be repeated after an uncertain interruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrySafety {
    /// Repeating the job has no additional external effect.
    Idempotent,
    /// The job may have changed state at an external provider; an interrupted attempt
    /// becomes UNKNOWN_OUTCOME and waits for reconciliation.
    ReconcileOnUncertainty,
}

impl RetrySafety {
    pub fn as_str(&self) -> &'static str {
        match self {
            RetrySafety::Idempotent => "IDEMPOTENT",
            RetrySafety::ReconcileOnUncertainty => "RECONCILE_ON_UNCERTAINTY",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: &'static str,
    pub dedupe_key: Option<String>,
    pub payload: Value,
    pub retry_safety: RetrySafety,
    pub max_attempts: i32,
    pub run_at: DateTime<Utc>,
}

/// Records `job` in the caller's transaction. Returns `(job id, newly created)`; an
/// existing job with the same `(kind, dedupe_key)` is returned instead of a duplicate.
pub async fn enqueue<C: ConnectionTrait>(
    db: &C,
    job: NewJob,
    ctx: &RequestContext,
    now: DateTime<Utc>,
) -> Result<(Uuid, bool), DbErr> {
    let id = Uuid::new_v4();
    let inserted = db
        .query_one(stmt(
            "INSERT INTO numbering_jobs (id, kind, dedupe_key, payload, retry_safety, correlation_id, causation_id, \
               max_attempts, next_attempt_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) \
             ON CONFLICT (kind, dedupe_key) DO NOTHING RETURNING id",
            vec![
                id.into(),
                job.kind.into(),
                job.dedupe_key.clone().into(),
                job.payload.into(),
                job.retry_safety.as_str().into(),
                ctx.correlation_id.as_str().into(),
                ctx.cause().as_str().into(),
                job.max_attempts.into(),
                job.run_at.into(),
                now.into(),
            ],
        ))
        .await?;
    if inserted.is_some() {
        return Ok((id, true));
    }
    let existing = db
        .query_one(stmt(
            "SELECT id FROM numbering_jobs WHERE kind = $1 AND dedupe_key = $2",
            vec![job.kind.into(), job.dedupe_key.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("deduplicated job".into()))?;
    Ok((existing.try_get_by_index(0)?, false))
}

/// A claimed job.
#[derive(Debug, Clone, FromQueryResult)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub retry_safety: String,
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub attempts: i32,
    pub max_attempts: i32,
}

impl ClaimedJob {
    /// The context in which the job's effects are recorded: same correlation, caused by
    /// this job.
    pub fn context(&self) -> RequestContext {
        let correlation = cf_service_auth::CorrelationId::parse(&self.correlation_id)
            .unwrap_or_else(|_| cf_service_auth::CorrelationId::generate());
        RequestContext::continuing(
            correlation,
            crate::domain::CausationId::of_record("job", self.id),
        )
    }
}

/// What a handler reports for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    Succeeded,
    /// Transient: retried with backoff until the attempt cap, then DEAD_LETTER.
    Retry {
        code: &'static str,
        message: String,
    },
    /// Permanent: FAILED, never retried.
    Failed {
        code: &'static str,
        message: String,
    },
    /// The external effect is unknown: UNKNOWN_OUTCOME, never retried automatically.
    Unknown {
        code: &'static str,
        message: String,
    },
}

#[async_trait::async_trait]
pub trait JobHandler: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn run(&self, job: &ClaimedJob) -> JobOutcome;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunReport {
    pub recovered_for_retry: usize,
    pub parked_unknown: usize,
    pub claimed: usize,
    pub succeeded: usize,
    pub retried: usize,
    pub failed: usize,
    pub dead_lettered: usize,
    pub unknown: usize,
    pub lost_claims: usize,
}

pub struct JobRunner {
    db: DatabaseConnection,
    clock: SharedClock,
    owner: String,
    lease: chrono::Duration,
    retry: RetryPolicy,
    batch: u64,
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
}

impl JobRunner {
    /// `owner` identifies this worker instance in leases (bounded, no secrets).
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        owner: impl Into<String>,
        lease: chrono::Duration,
        retry: RetryPolicy,
    ) -> Self {
        Self {
            db,
            clock,
            owner: bounded_error(&owner.into()).chars().take(100).collect(),
            lease,
            retry,
            batch: 20,
            handlers: HashMap::new(),
        }
    }

    pub fn with_handler(mut self, handler: Arc<dyn JobHandler>) -> Self {
        self.handlers.insert(handler.kind(), handler);
        self
    }

    pub fn with_batch(mut self, batch: u64) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// Recovers claims whose lease expired, by retry safety (see the module docs).
    pub async fn recover_expired(&self, report: &mut RunReport) -> Result<(), DbErr> {
        let now = self.clock.now();
        let parked = self
            .db
            .execute(stmt(
                "UPDATE numbering_jobs SET status = 'UNKNOWN_OUTCOME', lease_token = NULL, lease_owner = NULL, \
                   lease_expires_at = NULL, completed_at = $1, updated_at = $1, \
                   last_error_code = 'INTERRUPTED_UNCERTAIN', \
                   last_error = 'the worker stopped while the job may have reached an external provider' \
                 WHERE status = 'RUNNING' AND lease_expires_at <= $1 AND retry_safety = 'RECONCILE_ON_UNCERTAINTY'",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        let exhausted = self
            .db
            .execute(stmt(
                "UPDATE numbering_jobs SET status = 'DEAD_LETTER', lease_token = NULL, lease_owner = NULL, \
                   lease_expires_at = NULL, completed_at = $1, updated_at = $1, last_error_code = 'INTERRUPTED' \
                 WHERE status = 'RUNNING' AND lease_expires_at <= $1 AND retry_safety = 'IDEMPOTENT' \
                   AND attempts >= max_attempts",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        let retried = self
            .db
            .execute(stmt(
                "UPDATE numbering_jobs SET status = 'PENDING', lease_token = NULL, lease_owner = NULL, \
                   lease_expires_at = NULL, next_attempt_at = $1, updated_at = $1, last_error_code = 'INTERRUPTED' \
                 WHERE status = 'RUNNING' AND lease_expires_at <= $1 AND retry_safety = 'IDEMPOTENT' \
                   AND attempts < max_attempts",
                vec![now.into()],
            ))
            .await?
            .rows_affected();
        report.parked_unknown += parked as usize;
        report.dead_lettered += exhausted as usize;
        report.recovered_for_retry += retried as usize;
        if parked > 0 {
            crate::metrics::inc("numbering_jobs_parked_unknown_total", &[]);
            tracing::error!(
                jobs = parked,
                "interrupted provider-affecting jobs parked as UNKNOWN_OUTCOME"
            );
        }
        Ok(())
    }

    /// Claims due jobs of the registered kinds under `token`.
    pub async fn claim(&self, token: Uuid) -> Result<Vec<ClaimedJob>, DbErr> {
        let now = self.clock.now();
        let kinds: Vec<String> = self.handlers.keys().map(|k| k.to_string()).collect();
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        ClaimedJob::find_by_statement(stmt(
            "UPDATE numbering_jobs j SET status = 'RUNNING', attempts = j.attempts + 1, lease_token = $1, \
               lease_owner = $2, lease_expires_at = $3, updated_at = $4 \
             WHERE j.id IN ( \
               SELECT id FROM numbering_jobs \
                WHERE status = 'PENDING' AND kind = ANY($5) AND next_attempt_at <= $4 AND attempts < max_attempts \
                ORDER BY next_attempt_at, created_at LIMIT $6 FOR UPDATE SKIP LOCKED) \
             RETURNING j.id, j.kind, j.payload, j.retry_safety, j.correlation_id, j.causation_id, j.attempts, j.max_attempts",
            vec![
                token.into(),
                self.owner.clone().into(),
                (now + self.lease).into(),
                now.into(),
                kinds.into(),
                (self.batch as i64).into(),
            ],
        ))
        .all(&self.db)
        .await
    }

    /// Records the outcome of a claimed attempt (guarded by `token`).
    pub async fn finish(
        &self,
        job: &ClaimedJob,
        token: Uuid,
        outcome: &JobOutcome,
        report: &mut RunReport,
    ) -> Result<(), DbErr> {
        let now = self.clock.now();
        let guard = "WHERE id = $1 AND lease_token = $2 AND status = 'RUNNING'";
        let release =
            "lease_token = NULL, lease_owner = NULL, lease_expires_at = NULL, updated_at = $3";
        let (sql, values, label) = match outcome {
            JobOutcome::Succeeded => (
                format!("UPDATE numbering_jobs SET status = 'SUCCEEDED', completed_at = $3, {release} {guard}"),
                vec![job.id.into(), token.into(), now.into()],
                "succeeded",
            ),
            JobOutcome::Failed { code, message } => (
                format!(
                    "UPDATE numbering_jobs SET status = 'FAILED', completed_at = $3, last_error_code = $4, \
                       last_error = $5, {release} {guard}"
                ),
                vec![job.id.into(), token.into(), now.into(), (*code).into(), bounded_error(message).into()],
                "failed",
            ),
            JobOutcome::Unknown { code, message } => (
                format!(
                    "UPDATE numbering_jobs SET status = 'UNKNOWN_OUTCOME', completed_at = $3, last_error_code = $4, \
                       last_error = $5, {release} {guard}"
                ),
                vec![job.id.into(), token.into(), now.into(), (*code).into(), bounded_error(message).into()],
                "unknown",
            ),
            JobOutcome::Retry { code, message } if job.attempts >= job.max_attempts => (
                format!(
                    "UPDATE numbering_jobs SET status = 'DEAD_LETTER', completed_at = $3, last_error_code = $4, \
                       last_error = $5, {release} {guard}"
                ),
                vec![job.id.into(), token.into(), now.into(), (*code).into(), bounded_error(message).into()],
                "dead_letter",
            ),
            JobOutcome::Retry { code, message } => (
                format!(
                    "UPDATE numbering_jobs SET status = 'PENDING', next_attempt_at = $6, last_error_code = $4, \
                       last_error = $5, {release} {guard}"
                ),
                vec![
                    job.id.into(),
                    token.into(),
                    now.into(),
                    (*code).into(),
                    bounded_error(message).into(),
                    (now + self.retry.delay(job.attempts)).into(),
                ],
                "retry",
            ),
        };
        let updated = self.db.execute(stmt(&sql, values)).await?.rows_affected();
        if updated != 1 {
            report.lost_claims += 1;
            tracing::warn!(job_id = %job.id, kind = %job.kind, "job claim lost before completion");
            return Ok(());
        }
        match label {
            "succeeded" => report.succeeded += 1,
            "failed" => report.failed += 1,
            "unknown" => report.unknown += 1,
            "dead_letter" => report.dead_lettered += 1,
            _ => report.retried += 1,
        }
        crate::metrics::inc("numbering_job_attempts_total", &[("outcome", label)]);
        Ok(())
    }

    /// One worker pass: recover expired leases, claim due jobs, run them.
    pub async fn run_due(&self) -> Result<RunReport, DbErr> {
        let mut report = RunReport::default();
        self.recover_expired(&mut report).await?;
        let token = Uuid::new_v4();
        let jobs = self.claim(token).await?;
        report.claimed = jobs.len();
        for job in &jobs {
            let outcome = match self.handlers.get(job.kind.as_str()) {
                Some(handler) => {
                    let ctx = job.context();
                    crate::domain::scoped(ctx, handler.run(job)).await
                }
                None => JobOutcome::Failed {
                    code: "NO_HANDLER",
                    message: "no handler is registered for this job kind".into(),
                },
            };
            self.finish(job, token, &outcome, &mut report).await?;
        }
        Ok(report)
    }

    /// Runs worker passes every `interval` until the process stops.
    pub async fn run(self: Arc<Self>, interval: std::time::Duration) {
        loop {
            if let Err(e) = self.run_due().await {
                tracing::warn!(summary = %crate::numbers::error::summarize_db_error(&e), "job pass failed");
            }
            tokio::time::sleep(interval).await;
        }
    }
}

/// Reconciliation's resolution of an UNKNOWN_OUTCOME job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The external effect is confirmed to have happened.
    Happened,
    /// The external effect is confirmed NOT to have happened.
    DidNotHappen,
}

/// Records reconciliation's decision on an uncertain job. Returns `false` when the job is
/// not UNKNOWN_OUTCOME (nothing changes).
pub async fn resolve_unknown<C: ConnectionTrait>(
    db: &C,
    job_id: Uuid,
    resolution: Resolution,
    now: DateTime<Utc>,
) -> Result<bool, DbErr> {
    let (status, code) = match resolution {
        Resolution::Happened => ("SUCCEEDED", "RECONCILED_HAPPENED"),
        Resolution::DidNotHappen => ("FAILED", "RECONCILED_NOT_HAPPENED"),
    };
    let updated = db
        .execute(stmt(
            "UPDATE numbering_jobs SET status = $2, last_error_code = $3, updated_at = $4, completed_at = $4 \
             WHERE id = $1 AND status = 'UNKNOWN_OUTCOME'",
            vec![job_id.into(), status.into(), code.into(), now.into()],
        ))
        .await?
        .rows_affected();
    Ok(updated == 1)
}
