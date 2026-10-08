//! Active-call enforcement (Part D §29): Privacy Relay → Calling.
//!
//! The hard security boundary is NOT this module: Calling asks Privacy Relay for current
//! authority before every protected call and transfer, so a removal / revocation denies
//! new communication the moment its transaction commits. This module handles calls that
//! are ALREADY active when policy says they must end:
//!
//! 1. the authority change persists a durable `privacy_relay_enforcement_jobs` row (and an
//!    ENFORCEMENT_REQUESTED audit event) in its own transaction;
//! 2. after commit, delivery is attempted synchronously through the authenticated Calling
//!    enforcement API (`POST /internal/v1/privacy-relay/enforcement`, service token with
//!    the delegated platform context of the context's owner);
//! 3. on failure the job keeps a backoff schedule and is retried by any instance under a
//!    `FOR UPDATE SKIP LOCKED` lease; APPLIED only when Calling confirms.
//!
//! The command carries ids, versions, a reason and the terminate flag — never contacts or
//! provider data. Enforcement is restrictive only: it can terminate protected calls,
//! never grant authority.

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{DatabaseTransaction, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    audit::{self, AuditDraft},
    db,
    error::RelayResult,
    models::{EnforcementJobRow, ENFORCEMENT_JOB_COLUMNS},
    scope::{environment_kind_str, parse_environment_kind, CommandContext, RelayScope},
    service::RelayService,
    vocabulary::{AuditEventType, EnforcementReason, EnforcementState},
};

/// Calling's enforcement route.
pub const ENFORCEMENT_PATH: &str = "/internal/v1/privacy-relay/enforcement";
/// Lease held by an instance while it delivers a job.
const LEASE: i64 = 60;

/// One enforcement delivery.
#[derive(Debug, Clone)]
pub struct EnforcementDelivery {
    pub delegated: DelegatedPlatformContext,
    pub correlation_id: CorrelationId,
    pub payload: Value,
}

/// Calling's confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementAck {
    pub result: Value,
}

/// Normalized delivery failure (no URL, token or body ever surfaces).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryFailure {
    Timeout,
    Unavailable,
    Rejected,
    InvalidResponse,
    NotConfigured,
}

impl DeliveryFailure {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryFailure::Timeout => "TIMEOUT",
            DeliveryFailure::Unavailable => "UNAVAILABLE",
            DeliveryFailure::Rejected => "REJECTED",
            DeliveryFailure::InvalidResponse => "INVALID_RESPONSE",
            DeliveryFailure::NotConfigured => "NOT_CONFIGURED",
        }
    }
}

/// Provider-neutral Calling enforcement port.
#[async_trait]
pub trait CallingEnforcer: Send + Sync {
    async fn enforce(
        &self,
        delivery: &EnforcementDelivery,
    ) -> Result<EnforcementAck, DeliveryFailure>;
}

/// No Calling endpoint (operator tooling): every delivery stays durably PENDING.
pub struct UnconfiguredCallingEnforcer;

#[async_trait]
impl CallingEnforcer for UnconfiguredCallingEnforcer {
    async fn enforce(&self, _: &EnforcementDelivery) -> Result<EnforcementAck, DeliveryFailure> {
        Err(DeliveryFailure::NotConfigured)
    }
}

/// The authenticated HTTP adapter (service token, delegated context, correlation id).
pub struct HttpCallingEnforcer {
    client: Arc<InternalServiceClient>,
}

impl HttpCallingEnforcer {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl CallingEnforcer for HttpCallingEnforcer {
    async fn enforce(
        &self,
        delivery: &EnforcementDelivery,
    ) -> Result<EnforcementAck, DeliveryFailure> {
        let request =
            OutboundServiceRequest::continuing(ServiceId::calling(), &delivery.correlation_id)
                .with_delegated_context(delivery.delegated.clone());
        let response = self
            .client
            .post_json(&request, ENFORCEMENT_PATH, &delivery.payload)
            .await
            .map_err(|e| match e {
                InternalCallError::UnknownTarget(_) => DeliveryFailure::NotConfigured,
                InternalCallError::Timeout { .. } => DeliveryFailure::Timeout,
                InternalCallError::Connect { .. } | InternalCallError::Transport { .. } => {
                    DeliveryFailure::Unavailable
                }
                InternalCallError::Status { status, .. } if status >= 500 => {
                    DeliveryFailure::Unavailable
                }
                InternalCallError::Status { .. } => DeliveryFailure::Rejected,
                _ => DeliveryFailure::InvalidResponse,
            })?;
        let body: Value = response
            .json()
            .map_err(|_| DeliveryFailure::InvalidResponse)?;
        if body.get("status").and_then(Value::as_str) != Some("APPLIED")
            || body.get("enforcementEventId") != delivery.payload.get("enforcementEventId")
        {
            return Err(DeliveryFailure::InvalidResponse);
        }
        Ok(EnforcementAck {
            result: json!({
                "status": "APPLIED",
                "terminated_calls": body.get("terminatedCalls").and_then(Value::as_u64).unwrap_or(0),
                "matched_calls": body.get("matchedCalls").and_then(Value::as_u64).unwrap_or(0),
            }),
        })
    }
}

/// Inputs of a new enforcement job.
pub struct NewJob<'a> {
    pub context_id: Uuid,
    pub context_version: i64,
    pub context_status: &'static str,
    pub session: Option<(Uuid, i64)>,
    pub reason: EnforcementReason,
    pub affected_principals: &'a [Uuid],
    pub terminate_active_calls: bool,
}

/// Persists a job + ENFORCEMENT_REQUESTED inside the authority change's transaction
/// (the caller holds the context row lock). Returns the enforcement event id.
pub async fn request(
    txn: &DatabaseTransaction,
    cmd: &CommandContext,
    runtime: cf_service_auth::RuntimeEnvironment,
    job: NewJob<'_>,
    now: DateTime<Utc>,
) -> RelayResult<Uuid> {
    let scope = cmd.scope();
    let id = Uuid::new_v4();
    let event_id = Uuid::new_v4();
    let mut payload = json!({
        "enforcementEventId": event_id,
        "contextId": job.context_id,
        "contextVersion": job.context_version,
        "relaySessionIds": job.session.map(|s| vec![s.0]).unwrap_or_default(),
        "reason": job.reason.as_str(),
        "affectedPrincipalIds": job.affected_principals,
        "terminateActiveCalls": job.terminate_active_calls,
    });
    if let Some((_, session_version)) = job.session {
        // Calls authorized at this session version or later already reflect the change.
        payload["relaySessionVersion"] = json!(session_version);
    }
    db::exec(
        txn,
        "INSERT INTO privacy_relay_enforcement_jobs (id, enforcement_event_id, tenant_id, application_id, \
           environment_id, environment_kind, on_behalf_of_service_account_id, context_id, context_version, \
           relay_session_id, session_version, reason, affected_principal_refs, terminate_active_calls, payload, \
           status, attempt_count, next_attempt_at, request_id, correlation_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, 'PENDING', 0, $16, $17, $18, $16, $16)",
        vec![
            id.into(),
            event_id.into(),
            scope.tenant_id().into(),
            scope.application_id().into(),
            scope.environment_id().into(),
            environment_kind_str(cmd.environment_kind()).into(),
            cmd.service_account_id().into(),
            job.context_id.into(),
            job.context_version.into(),
            job.session.map(|s| s.0).into(),
            job.session.map(|s| s.1).into(),
            job.reason.as_str().into(),
            job.affected_principals.to_vec().into(),
            job.terminate_active_calls.into(),
            payload.into(),
            now.into(),
            cmd.request_id().as_str().into(),
            cmd.correlation_id().as_str().into(),
        ],
    )
    .await?;
    audit::append(
        txn,
        cmd,
        runtime,
        AuditDraft {
            event_type: AuditEventType::EnforcementRequested,
            context_id: job.context_id,
            context_version: job.context_version,
            context_status: job.context_status,
            session: job.session,
            session_status: None,
            enforcement_job_id: Some(id),
            data: audit::data(vec![
                ("enforcement_event_id", json!(event_id)),
                ("reason", json!(job.reason.as_str())),
                ("terminate_active_calls", json!(job.terminate_active_calls)),
                (
                    "affected_principal_count",
                    json!(job.affected_principals.len()),
                ),
            ]),
        },
        now,
    )
    .await?;
    Ok(event_id)
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds(2_i64.saturating_pow(attempts.clamp(0, 9) as u32).min(300))
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct ContextStatusRow {
    status: String,
    version: i64,
}

impl RelayService {
    /// Synchronous-first delivery of one job (by enforcement event id). Returns the job
    /// state afterwards; PENDING when Calling did not confirm (the job stays durable).
    pub async fn deliver_enforcement(&self, event_id: Uuid) -> RelayResult<EnforcementState> {
        let now = self.now();
        let claimed: Option<EnforcementJobRow> = db::one(
            &self.db,
            &format!(
                "UPDATE privacy_relay_enforcement_jobs SET lease_owner = $2, lease_expires_at = $3, updated_at = $4 \
                 WHERE id = (SELECT id FROM privacy_relay_enforcement_jobs \
                             WHERE enforcement_event_id = $1 AND status = 'PENDING' \
                               AND (lease_expires_at IS NULL OR lease_expires_at <= $4) \
                             FOR UPDATE SKIP LOCKED) \
                 RETURNING {ENFORCEMENT_JOB_COLUMNS}"
            ),
            vec![
                event_id.into(),
                self.worker_id.clone().into(),
                (now + Duration::seconds(LEASE)).into(),
                now.into(),
            ],
        )
        .await?;
        match claimed {
            Some(job) => self.attempt(job).await,
            None => {
                let status: Option<JobState> = db::one(
                    &self.db,
                    "SELECT status FROM privacy_relay_enforcement_jobs WHERE enforcement_event_id = $1",
                    vec![event_id.into()],
                )
                .await?;
                Ok(match status.map(|s| s.status) {
                    Some(s) if s == "APPLIED" => EnforcementState::Applied,
                    _ => EnforcementState::Pending,
                })
            }
        }
    }

    /// Retries due jobs (multi-instance safe: SKIP LOCKED leases). Returns how many were
    /// applied.
    pub async fn run_enforcement_sweep(&self, limit: u64) -> RelayResult<usize> {
        let now = self.now();
        let claimed: Vec<EnforcementJobRow> = db::all(
            &self.db,
            &format!(
                "UPDATE privacy_relay_enforcement_jobs SET lease_owner = $1, lease_expires_at = $2, updated_at = $3 \
                 WHERE id IN (SELECT id FROM privacy_relay_enforcement_jobs \
                              WHERE status = 'PENDING' AND next_attempt_at <= $3 \
                                AND (lease_expires_at IS NULL OR lease_expires_at <= $3) \
                              ORDER BY next_attempt_at LIMIT $4 FOR UPDATE SKIP LOCKED) \
                 RETURNING {ENFORCEMENT_JOB_COLUMNS}"
            ),
            vec![
                self.worker_id.clone().into(),
                (now + Duration::seconds(LEASE)).into(),
                now.into(),
                (limit as i64).into(),
            ],
        )
        .await?;
        let mut applied = 0;
        for job in claimed {
            if self.attempt(job).await? == EnforcementState::Applied {
                applied += 1;
            }
        }
        Ok(applied)
    }

    async fn attempt(&self, job: EnforcementJobRow) -> RelayResult<EnforcementState> {
        let scope = RelayScope::from_row(job.tenant_id, job.application_id, job.environment_id);
        let Some(kind) = parse_environment_kind(&job.environment_kind) else {
            return Ok(EnforcementState::Pending);
        };
        let correlation =
            CorrelationId::parse(&job.correlation_id).unwrap_or_else(|_| CorrelationId::generate());
        let delegated = DelegatedPlatformContext::new(
            job.tenant_id,
            job.application_id,
            job.environment_id,
            kind,
            job.on_behalf_of_service_account_id,
        );
        let outcome = self
            .enforcer
            .enforce(&EnforcementDelivery {
                delegated,
                correlation_id: correlation.clone(),
                payload: job.payload.clone(),
            })
            .await;
        let cmd = CommandContext::system(scope, kind, job.on_behalf_of_service_account_id)
            .with_correlation(correlation);
        let now = self.now();
        let txn = self.db.begin().await?;
        // Context lock first (same order as every command), then the job.
        let ctx: Option<ContextStatusRow> = db::one(
            &txn,
            "SELECT status, version FROM communication_contexts WHERE id = $1 FOR UPDATE",
            vec![job.context_id.into()],
        )
        .await?;
        let Some(ctx) = ctx else {
            txn.rollback().await?;
            return Ok(EnforcementState::Pending);
        };
        let context_status = crate::vocabulary::CommunicationContextStatus::parse(&ctx.status)
            .map(|s| s.as_str())
            .unwrap_or("ACTIVE");
        let session = match (job.relay_session_id, job.session_version) {
            (Some(id), Some(version)) => Some((id, version)),
            _ => None,
        };
        let state = match outcome {
            Ok(ack) => {
                let updated = db::exec(
                    &txn,
                    "UPDATE privacy_relay_enforcement_jobs SET status = 'APPLIED', completed_at = $3, \
                       result_snapshot = $4, lease_owner = NULL, lease_expires_at = NULL, \
                       attempt_count = attempt_count + 1, last_error_category = NULL, updated_at = $3 \
                     WHERE id = $1 AND lease_owner = $2 AND status = 'PENDING'",
                    vec![
                        job.id.into(),
                        self.worker_id.clone().into(),
                        now.into(),
                        ack.result.clone().into(),
                    ],
                )
                .await?;
                if updated == 1 {
                    audit::append(
                        &txn,
                        &cmd,
                        self.runtime,
                        AuditDraft {
                            event_type: AuditEventType::EnforcementApplied,
                            context_id: job.context_id,
                            context_version: ctx.version,
                            context_status,
                            session,
                            session_status: None,
                            enforcement_job_id: Some(job.id),
                            data: audit::data(vec![
                                ("enforcement_event_id", json!(job.enforcement_event_id)),
                                ("reason", json!(job.reason)),
                                ("attempt", json!(job.attempt_count + 1)),
                                ("result", ack.result),
                            ]),
                        },
                        now,
                    )
                    .await?;
                }
                tracing::info!(
                    context_id = %job.context_id,
                    enforcement_event_id = %job.enforcement_event_id,
                    "active-call enforcement applied"
                );
                EnforcementState::Applied
            }
            Err(failure) => {
                let attempts = job.attempt_count + 1;
                db::exec(
                    &txn,
                    "UPDATE privacy_relay_enforcement_jobs SET attempt_count = $3, next_attempt_at = $4, \
                       lease_owner = NULL, lease_expires_at = NULL, last_error_category = $5, updated_at = $6 \
                     WHERE id = $1 AND lease_owner = $2 AND status = 'PENDING'",
                    vec![
                        job.id.into(),
                        self.worker_id.clone().into(),
                        attempts.into(),
                        (now + backoff(attempts)).into(),
                        failure.as_str().into(),
                        now.into(),
                    ],
                )
                .await?;
                if attempts == 1 {
                    audit::append(
                        &txn,
                        &cmd,
                        self.runtime,
                        AuditDraft {
                            event_type: AuditEventType::EnforcementFailed,
                            context_id: job.context_id,
                            context_version: ctx.version,
                            context_status,
                            session,
                            session_status: None,
                            enforcement_job_id: Some(job.id),
                            data: audit::data(vec![
                                ("enforcement_event_id", json!(job.enforcement_event_id)),
                                ("reason", json!(job.reason)),
                                ("error_category", json!(failure.as_str())),
                                ("retrying", json!(true)),
                            ]),
                        },
                        now,
                    )
                    .await?;
                }
                tracing::warn!(
                    context_id = %job.context_id,
                    enforcement_event_id = %job.enforcement_event_id,
                    attempt = attempts,
                    error_category = failure.as_str(),
                    "active-call enforcement not confirmed; will retry"
                );
                EnforcementState::Pending
            }
        };
        txn.commit().await?;
        Ok(state)
    }
}

#[derive(Debug, sea_orm::FromQueryResult)]
struct JobState {
    status: String,
}
