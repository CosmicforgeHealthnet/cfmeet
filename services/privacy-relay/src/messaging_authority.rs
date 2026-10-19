//! Part V H §103 — delivery of communication-context authority to Messaging.
//!
//! Jobs are enqueued by the database itself at commit of every context version change
//! (`trg_privacy_relay_enqueue_messaging_authority`). This worker delivers them,
//! multi-instance safe (SKIP LOCKED leases), with capped exponential backoff, to
//! `POST /internal/v1/context-authorities` on Messaging — signed with Privacy Relay's
//! service key and the context owner's delegated platform context (Messaging grants the
//! call `messaging-context-authority.apply`). The body carries the opaque context id, the
//! authority state and the context version (Messaging's fence) — never a contact value.
//! A stale answer (`applied: false, STALE_VERSION`) is success: a newer version was
//! already applied. Delivery order therefore never matters for correctness: Messaging's
//! fence keeps the newest version whatever arrives last.

use std::sync::Arc;

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
};
use chrono::Duration;
use sea_orm::FromQueryResult;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    db, error::RelayResult, metrics, scope::parse_environment_kind, service::RelayService,
};

/// Messaging's route.
pub const CONTEXT_AUTHORITY_PATH: &str = "/internal/v1/context-authorities";
/// The namespace Privacy Relay owns in Messaging.
pub const NAMESPACE: &str = "privacy-relay";
const LEASE_SECONDS: i64 = 60;
/// Attempts before a job is dead-lettered (~1 hour with the capped backoff).
pub const MAX_ATTEMPTS: i32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushFailure {
    Timeout,
    Unavailable,
    Rejected,
    InvalidResponse,
    NotConfigured,
}

impl PushFailure {
    pub fn category(&self) -> &'static str {
        match self {
            PushFailure::Timeout => "TIMEOUT",
            PushFailure::Unavailable => "UNAVAILABLE",
            PushFailure::Rejected => "REJECTED",
            PushFailure::InvalidResponse => "INVALID_RESPONSE",
            PushFailure::NotConfigured => "NOT_CONFIGURED",
        }
    }
}

#[async_trait]
pub trait MessagingAuthorityPusher: Send + Sync {
    async fn push(
        &self,
        delegated: &DelegatedPlatformContext,
        body: &Value,
    ) -> Result<Value, PushFailure>;
}

/// Messaging not configured: jobs stay PENDING (durable) until it is.
pub struct UnconfiguredMessagingAuthority;

#[async_trait]
impl MessagingAuthorityPusher for UnconfiguredMessagingAuthority {
    async fn push(&self, _: &DelegatedPlatformContext, _: &Value) -> Result<Value, PushFailure> {
        Err(PushFailure::NotConfigured)
    }
}

/// Signed internal HTTP to Messaging.
pub struct HttpMessagingAuthority {
    client: Arc<InternalServiceClient>,
}

impl HttpMessagingAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MessagingAuthorityPusher for HttpMessagingAuthority {
    async fn push(
        &self,
        delegated: &DelegatedPlatformContext,
        body: &Value,
    ) -> Result<Value, PushFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::messaging())
            .with_delegated_context(delegated.clone());
        match self
            .client
            .post_json(&request, CONTEXT_AUTHORITY_PATH, body)
            .await
        {
            Ok(r) => {
                let v: Value = r.json().map_err(|_| PushFailure::InvalidResponse)?;
                match v.get("applied").and_then(Value::as_bool) {
                    Some(true) => Ok(v),
                    Some(false) if v["reason"] == "STALE_VERSION" => Ok(v),
                    _ => Err(PushFailure::InvalidResponse),
                }
            }
            Err(InternalCallError::UnknownTarget(_)) => Err(PushFailure::NotConfigured),
            Err(InternalCallError::Timeout { .. }) => Err(PushFailure::Timeout),
            Err(InternalCallError::Status { status, .. }) if status >= 500 || status == 429 => {
                Err(PushFailure::Unavailable)
            }
            Err(InternalCallError::Status { .. }) => Err(PushFailure::Rejected),
            Err(_) => Err(PushFailure::Unavailable),
        }
    }
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct AuthorityJobRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: String,
    pub on_behalf_of_service_account_id: Uuid,
    pub context_id: Uuid,
    pub context_version: i64,
    pub authority_state: String,
    pub reason: String,
    pub attempt_count: i32,
}

const COLUMNS: &str = "id, tenant_id, application_id, environment_id, environment_kind, \
     on_behalf_of_service_account_id, context_id, context_version, authority_state, reason, attempt_count";

/// The request Messaging receives for a job (ids and states only).
pub fn request_body(job: &AuthorityJobRow) -> Value {
    json!({
        "namespace": NAMESPACE,
        "reference": job.context_id.hyphenated().to_string(),
        "state": job.authority_state,
        "sourceVersion": job.context_version,
        "reason": job.reason,
    })
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

impl RelayService {
    /// Delivers due jobs in context-version order. Returns how many were applied.
    pub async fn run_messaging_authority_sweep(&self, limit: u64) -> RelayResult<usize> {
        // Jobs are enqueued by the database (its clock), so due times use it too.
        let claimed: Vec<AuthorityJobRow> = db::all(
            &self.db,
            &format!(
                "UPDATE privacy_relay_messaging_authority_jobs SET lease_owner = $1, \
                   lease_expires_at = now() + make_interval(secs => $2), updated_at = now() \
                 WHERE id IN (SELECT id FROM privacy_relay_messaging_authority_jobs \
                              WHERE status = 'PENDING' AND next_attempt_at <= now() \
                                AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
                              ORDER BY context_id, context_version LIMIT $3 FOR UPDATE SKIP LOCKED) \
                 RETURNING {COLUMNS}"
            ),
            vec![
                self.worker_id.clone().into(),
                (LEASE_SECONDS as f64).into(),
                (limit as i64).into(),
            ],
        )
        .await?;
        let mut claimed = claimed;
        claimed.sort_by_key(|j| (j.context_id, j.context_version));
        let mut applied = 0;
        for job in claimed {
            let Some(kind) = parse_environment_kind(&job.environment_kind) else {
                continue;
            };
            let delegated = DelegatedPlatformContext::new(
                job.tenant_id,
                job.application_id,
                job.environment_id,
                kind,
                job.on_behalf_of_service_account_id,
            );
            let outcome = self.messaging.push(&delegated, &request_body(&job)).await;
            match outcome {
                Ok(result) => {
                    let snapshot = json!({
                        "applied": result.get("applied").cloned().unwrap_or(Value::Null),
                        "reason": result.get("reason").cloned().unwrap_or(Value::Null),
                        "conversations_affected": result.get("conversationsAffected").cloned().unwrap_or(Value::Null),
                    });
                    db::exec(
                        &self.db,
                        "UPDATE privacy_relay_messaging_authority_jobs SET status = 'APPLIED', completed_at = now(), \
                           result_snapshot = $2, attempt_count = attempt_count + 1, last_error_category = NULL, \
                           lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
                         WHERE id = $1 AND status = 'PENDING'",
                        vec![job.id.into(), snapshot.into()],
                    )
                    .await?;
                    metrics::inc(
                        "privacy_relay_messaging_authority_total",
                        &[("outcome", "applied")],
                    );
                    applied += 1;
                }
                Err(failure) => {
                    let attempts = job.attempt_count + 1;
                    let dead = attempts >= MAX_ATTEMPTS;
                    db::exec(
                        &self.db,
                        "UPDATE privacy_relay_messaging_authority_jobs SET attempt_count = $2, last_error_category = $3, \
                           next_attempt_at = now() + make_interval(secs => $4), \
                           status = CASE WHEN $5 THEN 'DEAD_LETTER' ELSE status END, \
                           completed_at = CASE WHEN $5 THEN now() ELSE completed_at END, \
                           lease_owner = NULL, lease_expires_at = NULL, updated_at = now() \
                         WHERE id = $1 AND status = 'PENDING'",
                        vec![
                            job.id.into(),
                            attempts.into(),
                            failure.category().into(),
                            (backoff(attempts).num_seconds() as f64).into(),
                            dead.into(),
                        ],
                    )
                    .await?;
                    metrics::inc(
                        "privacy_relay_messaging_authority_total",
                        &[("outcome", if dead { "dead_letter" } else { "retry" })],
                    );
                    if dead {
                        tracing::error!(context_id = %job.context_id, "messaging authority job dead-lettered");
                    }
                }
            }
        }
        Ok(applied)
    }
}
