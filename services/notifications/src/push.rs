//! Push (roadmap N9; U8/U9, I3, W3 push part, table 13).
//!
//! * Registration (U8/U9): the device must be the caller's ACTIVE Account device (X1); the
//!   handle passes the structural checks and the adapter's checks (Web Push SSRF host
//!   allowlist), is sealed at rest, never echoed, and unique by keyed digest (a handle
//!   that moves to another device leaves the old row).
//! * Dispatch (W3): PUSH deliveries are sent through the ported adapters with metadata-only
//!   payloads; an `InvalidHandle` disables the endpoint; retries back off; nothing is sent
//!   after the notification expires.
//! * Call fast lane (I3): synchronous, ≤ 1 s, no queue and no retry; ringing, signaling and
//!   arbitration stay in Calling, which also covers failure through its WebSocket ring and
//!   recovery API.

use std::{sync::Arc, time::Duration as StdDuration};

use cf_push_transport::{adapters::validate_handle_shape, PushOutcome, PushProvider};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    crypto::DataKeys,
    db,
    metrics::{label, Heartbeats, Metrics},
};

/// First-party scope of account registrations (platform tenants register through the
/// Messaging facade with their own scope).
pub const FIRST_PARTY_SCOPE: Uuid = Uuid::nil();
pub const PLATFORMS: &[&str] = &["APNS", "APNS_VOIP", "FCM", "WEB_PUSH"];
const PUSH_LEASE_SECS: i64 = 60;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    pub platform: String,
    pub handle: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("database error")]
    Db,
}

impl From<DbErr> for RegisterError {
    fn from(_: DbErr) -> Self {
        RegisterError::Db
    }
}

/// The adapter platform for a registration platform (APNs VoIP uses the APNs adapter).
fn adapter_platform(platform: &str) -> &str {
    if platform == "APNS_VOIP" {
        "APNS"
    } else {
        platform
    }
}

/// U8: registers (or replaces) the endpoint of one device and platform. Returns the
/// `pushTargetRef` (the endpoint id); the handle is never returned.
pub async fn register<C: ConnectionTrait>(
    conn: &C,
    keys: &DataKeys,
    provider: Option<&dyn PushProvider>,
    account_ref: &str,
    device: Uuid,
    req: &RegisterRequest,
    local_test_allowed: bool,
) -> Result<Uuid, RegisterError> {
    let platform = req.platform.as_str();
    if !PLATFORMS.contains(&platform) && !(local_test_allowed && platform == "LOCAL_TEST") {
        return Err(RegisterError::Invalid(
            "platform is APNS, APNS_VOIP, FCM or WEB_PUSH",
        ));
    }
    let handle = req.handle.trim();
    if handle.is_empty() || handle.len() > 4096 {
        return Err(RegisterError::Invalid("handle is 1 to 4096 characters"));
    }
    validate_handle_shape(adapter_platform(platform), handle).map_err(RegisterError::Invalid)?;
    if let Some(p) = provider {
        if p.supports(adapter_platform(platform)) {
            p.validate_handle(adapter_platform(platform), handle)
                .map_err(RegisterError::Invalid)?;
        }
    }
    let digest = keys.digest("push-handle", &format!("{platform}\u{0}{handle}"));
    // A handle re-registered by another device leaves its old row (UNIQUE handle_digest).
    db::exec(
        conn,
        "DELETE FROM notif_push_endpoints WHERE handle_digest = $1 AND NOT (environment_id = $2 AND account_device_ref = $3 AND platform = $4)",
        vec![digest.clone().into(), FIRST_PARTY_SCOPE.into(), device.into(), platform.into()],
    )
    .await?;
    let id = Uuid::new_v4();
    let row = conn
        .query_one(db::stmt(
            "INSERT INTO notif_push_endpoints (id, tenant_id, application_id, environment_id, account_ref,
                 account_device_ref, platform, handle_sealed, handle_key_id, handle_digest)
             VALUES ($1, $2, $2, $2, $3, $4, $5, '', $6, $7)
             ON CONFLICT (environment_id, account_device_ref, platform) DO UPDATE SET
                 account_ref = EXCLUDED.account_ref, handle_digest = EXCLUDED.handle_digest,
                 handle_key_id = EXCLUDED.handle_key_id, enabled = true, disabled_reason = NULL, updated_at = now()
             RETURNING id",
            vec![id.into(), FIRST_PARTY_SCOPE.into(), account_ref.into(), device.into(), platform.into(), keys.kid().into(), digest.into()],
        ))
        .await?
        .ok_or(RegisterError::Db)?;
    let endpoint: Uuid = row.try_get("", "id").map_err(|_| RegisterError::Db)?;
    // Sealed under the final row id (a seal never opens for another endpoint).
    db::exec(
        conn,
        "UPDATE notif_push_endpoints SET handle_sealed = $2 WHERE id = $1",
        vec![
            endpoint.into(),
            keys.seal(handle, &format!("push:{endpoint}")).into(),
        ],
    )
    .await?;
    Ok(endpoint)
}

/// U9: idempotent removal.
pub async fn remove<C: ConnectionTrait>(
    conn: &C,
    account_ref: &str,
    device: Uuid,
    platform: Option<&str>,
) -> Result<u64, DbErr> {
    db::exec(
        conn,
        "DELETE FROM notif_push_endpoints WHERE environment_id = $1 AND account_ref = $2 AND account_device_ref = $3
            AND ($4::text IS NULL OR platform = $4)",
        vec![FIRST_PARTY_SCOPE.into(), account_ref.into(), device.into(), platform.map(str::to_string).into()],
    )
    .await
}

/// Disables every endpoint of a device (device revoked, session ended, suspension).
pub async fn disable_device<C: ConnectionTrait>(
    conn: &C,
    device: Uuid,
    reason: &str,
) -> Result<u64, DbErr> {
    db::exec(
        conn,
        "UPDATE notif_push_endpoints SET enabled = false, disabled_reason = $2, updated_at = now() WHERE account_device_ref = $1 AND enabled",
        vec![device.into(), reason.into()],
    )
    .await
}

#[derive(Debug, Clone, FromQueryResult)]
pub struct EndpointRef {
    pub id: Uuid,
    pub platform: String,
}

/// Enabled endpoints of an account (non-VoIP: call invitations use I3).
pub async fn endpoints_for<C: ConnectionTrait>(
    conn: &C,
    account_ref: &str,
) -> Result<Vec<EndpointRef>, DbErr> {
    db::all(
        conn,
        "SELECT id, platform FROM notif_push_endpoints WHERE account_ref = $1 AND enabled AND platform <> 'APNS_VOIP' ORDER BY id",
        vec![account_ref.into()],
    )
    .await
}

/// Pushes recorded for a recipient in the last hour (rule 12 cap).
pub async fn pushes_last_hour<C: ConnectionTrait>(conn: &C, recipient: &str) -> Result<i64, DbErr> {
    db::scalar_i64(
        conn,
        "SELECT count(*)::bigint AS n FROM notif_deliveries d JOIN notif_notifications n ON n.id = d.notification_id
          WHERE n.recipient_ref = $1 AND d.channel = 'PUSH' AND d.status IN ('SENT', 'DELIVERED', 'DISPATCHING')
            AND d.created_at > now() - interval '1 hour'",
        vec![recipient.into()],
    )
    .await
}

#[derive(Debug, FromQueryResult)]
struct ClaimedPush {
    id: Uuid,
    lease_id: Option<Uuid>,
    target_ref: String,
    attempts: i32,
    template_key: String,
    params: Value,
    expires_at: Option<DateTime<Utc>>,
    recipient_ref: String,
}

#[derive(Debug, FromQueryResult)]
struct EndpointRow {
    platform: String,
    handle_sealed: String,
    enabled: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PushTick {
    pub claimed: u64,
    pub sent: u64,
    pub retried: u64,
    pub failed: u64,
    pub expired: u64,
}

pub struct PushWorker {
    db: DatabaseConnection,
    keys: Arc<DataKeys>,
    provider: Arc<dyn PushProvider>,
    metrics: Arc<Metrics>,
}

impl PushWorker {
    pub fn new(
        db: DatabaseConnection,
        keys: Arc<DataKeys>,
        provider: Arc<dyn PushProvider>,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            db,
            keys,
            provider,
            metrics,
        }
    }

    async fn finish(
        &self,
        id: Uuid,
        lease: Option<Uuid>,
        status: &str,
        class: Option<&str>,
        attempt: i32,
        outcome: &str,
    ) -> Result<(), DbErr> {
        db::exec(
            &self.db,
            "UPDATE notif_deliveries SET status = $3, lease_id = NULL, lease_until = NULL, last_error_class = $4,
                    completed_at = CASE WHEN $3 IN ('SENT', 'FAILED', 'EXPIRED') THEN now() ELSE completed_at END
              WHERE id = $1 AND lease_id IS NOT DISTINCT FROM $2",
            vec![id.into(), lease.into(), status.into(), class.map(str::to_string).into()],
        )
        .await?;
        db::exec(
            &self.db,
            "INSERT INTO notif_delivery_attempts (delivery_id, attempt, started_at, finished_at, outcome, error_class)
             VALUES ($1, $2, now(), now(), $3, $4) ON CONFLICT DO NOTHING",
            vec![id.into(), attempt.into(), outcome.into(), class.map(str::to_string).into()],
        )
        .await?;
        Ok(())
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> Result<PushTick, DbErr> {
        let lease = Uuid::new_v4();
        let claimed: Vec<ClaimedPush> = db::all(
            &self.db,
            "UPDATE notif_deliveries d SET status = 'DISPATCHING', lease_id = $1,
                    lease_until = now() + make_interval(secs => $2), attempts = LEAST(d.attempts + 1, 64)
               FROM notif_notifications n
              WHERE n.id = d.notification_id AND d.id IN (
                    SELECT id FROM notif_deliveries
                     WHERE channel = 'PUSH' AND ((status IN ('PENDING', 'DEFERRED') AND next_attempt_at <= now())
                        OR (status = 'DISPATCHING' AND lease_until < now()))
                     ORDER BY lane = 'HIGH' DESC, next_attempt_at
                     LIMIT 16 FOR UPDATE SKIP LOCKED)
          RETURNING d.id, d.lease_id, d.target_ref, d.attempts, n.template_key, n.params, n.expires_at, n.recipient_ref",
            vec![lease.into(), PUSH_LEASE_SECS.into()],
        )
        .await?;
        let mut t = PushTick {
            claimed: claimed.len() as u64,
            ..Default::default()
        };
        for job in claimed {
            if job.expires_at.is_some_and(|e| e <= now) || job.attempts > 8 {
                self.finish(
                    job.id,
                    job.lease_id,
                    "EXPIRED",
                    None,
                    job.attempts,
                    "PERMANENT",
                )
                .await?;
                t.expired += 1;
                continue;
            }
            let Ok(endpoint) = Uuid::parse_str(&job.target_ref) else {
                self.finish(
                    job.id,
                    job.lease_id,
                    "FAILED",
                    Some("BAD_TARGET"),
                    job.attempts,
                    "PERMANENT",
                )
                .await?;
                t.failed += 1;
                continue;
            };
            let row: Option<EndpointRow> = db::one(
                &self.db,
                "SELECT platform, handle_sealed, enabled FROM notif_push_endpoints WHERE id = $1",
                vec![endpoint.into()],
            )
            .await?;
            let Some(row) = row.filter(|r| r.enabled) else {
                self.finish(
                    job.id,
                    job.lease_id,
                    "CANCELLED",
                    Some("ENDPOINT_DISABLED"),
                    job.attempts,
                    "PERMANENT",
                )
                .await?;
                t.failed += 1;
                continue;
            };
            let Ok(handle) = self
                .keys
                .open(&row.handle_sealed, &format!("push:{endpoint}"))
            else {
                self.finish(
                    job.id,
                    job.lease_id,
                    "FAILED",
                    Some("HANDLE_UNAVAILABLE"),
                    job.attempts,
                    "PERMANENT",
                )
                .await?;
                t.failed += 1;
                continue;
            };
            let data: std::collections::BTreeMap<String, Value> = job
                .params
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let version = crate::inbox::unread_count(&self.db, &job.recipient_ref)
                .await
                .unwrap_or(0);
            let payload = crate::templates::push_payload(&job.template_key, &data, version);
            let platform = adapter_platform(&row.platform).to_string();
            let outcome = match tokio::time::timeout(
                StdDuration::from_secs(20),
                self.provider.send(&platform, &handle, &payload),
            )
            .await
            {
                Ok(o) => o,
                Err(_) => PushOutcome::Retryable("timeout".into()),
            };
            let plat = format!("platform=\"{}\"", label(&row.platform));
            match outcome {
                PushOutcome::Sent => {
                    self.finish(job.id, job.lease_id, "SENT", None, job.attempts, "OK")
                        .await?;
                    self.metrics.inc("notif_push_sent_total", plat);
                    t.sent += 1;
                }
                PushOutcome::InvalidHandle => {
                    disable_endpoint(&self.db, endpoint, "INVALID_HANDLE").await?;
                    self.finish(
                        job.id,
                        job.lease_id,
                        "FAILED",
                        Some("INVALID_HANDLE"),
                        job.attempts,
                        "PERMANENT",
                    )
                    .await?;
                    self.metrics.inc(
                        "notif_push_failed_total",
                        format!("{plat},class=\"INVALID_HANDLE\""),
                    );
                    t.failed += 1;
                }
                PushOutcome::Permanent(_) => {
                    self.finish(
                        job.id,
                        job.lease_id,
                        "FAILED",
                        Some("PROVIDER_PERMANENT"),
                        job.attempts,
                        "PERMANENT",
                    )
                    .await?;
                    self.metrics.inc(
                        "notif_push_failed_total",
                        format!("{plat},class=\"PERMANENT\""),
                    );
                    t.failed += 1;
                }
                PushOutcome::Retryable(_) => {
                    let next = now + crate::email::backoff(job.attempts);
                    db::exec(
                        &self.db,
                        "UPDATE notif_deliveries SET status = 'PENDING', next_attempt_at = $3, lease_id = NULL, lease_until = NULL,
                                last_error_class = 'PROVIDER_RETRYABLE'
                          WHERE id = $1 AND lease_id IS NOT DISTINCT FROM $2",
                        vec![job.id.into(), job.lease_id.into(), next.into()],
                    )
                    .await?;
                    self.metrics.inc("notif_push_retried_total", plat);
                    t.retried += 1;
                }
            }
        }
        Ok(t)
    }

    pub async fn run(
        &self,
        poll: StdDuration,
        heartbeats: Arc<Heartbeats>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        tracing::info!(provider = self.provider.name(), "push dispatcher started");
        loop {
            if *shutdown.borrow() {
                break;
            }
            heartbeats.beat("push");
            let busy = match self.tick(Utc::now()).await {
                Ok(t) => t.claimed > 0,
                Err(e) => {
                    tracing::error!(summary = %db::summarize_db_error(&e), "push dispatcher tick failed");
                    false
                }
            };
            if !busy {
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = shutdown.changed() => {}
                }
            }
        }
    }
}

async fn disable_endpoint<C: ConnectionTrait>(
    conn: &C,
    endpoint: Uuid,
    reason: &str,
) -> Result<(), DbErr> {
    db::exec(
        conn,
        "UPDATE notif_push_endpoints SET enabled = false, disabled_reason = $2, updated_at = now() WHERE id = $1",
        vec![endpoint.into(), reason.into()],
    )
    .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallPushRequest {
    pub push_target_refs: Vec<Uuid>,
    pub payload: Value,
    pub expires_at: DateTime<Utc>,
}

/// I3: best-effort call push, bounded to one second overall, never queued or retried.
pub async fn call_push(
    conn: &DatabaseConnection,
    keys: &DataKeys,
    provider: Option<&Arc<dyn PushProvider>>,
    req: &CallPushRequest,
    now: DateTime<Utc>,
) -> Result<Value, DbErr> {
    let mut results = Vec::new();
    let expired = req.expires_at <= now;
    let rows: Vec<(Uuid, Option<EndpointRow>)> = {
        let mut v = Vec::new();
        for id in &req.push_target_refs {
            let row: Option<EndpointRow> = db::one(
                conn,
                "SELECT platform, handle_sealed, enabled FROM notif_push_endpoints WHERE id = $1",
                vec![(*id).into()],
            )
            .await?;
            v.push((*id, row));
        }
        v
    };
    // Metadata only: the call id travels through Calling's own channels (WS ring and
    // recovery API); the push only wakes the device.
    let payload = json!({ "v": 1, "type": "call" });
    let deadline = tokio::time::Instant::now() + StdDuration::from_millis(1000);
    for (id, row) in rows {
        let outcome = match (&row, provider) {
            _ if expired => "EXPIRED",
            (None, _) => "UNKNOWN_TARGET",
            (Some(r), _) if !r.enabled => "DISABLED",
            (Some(_), None) => "NOT_CONFIGURED",
            (Some(r), Some(p)) => {
                let platform = adapter_platform(&r.platform);
                if !p.supports(platform) {
                    "NOT_CONFIGURED"
                } else if let Ok(handle) = keys.open(&r.handle_sealed, &format!("push:{id}")) {
                    match tokio::time::timeout_at(deadline, p.send(platform, &handle, &payload))
                        .await
                    {
                        Ok(PushOutcome::Sent) => "SENT",
                        Ok(PushOutcome::InvalidHandle) => {
                            disable_endpoint(conn, id, "INVALID_HANDLE").await?;
                            "INVALID_HANDLE"
                        }
                        Ok(PushOutcome::Retryable(_)) => "RETRYABLE_FAILURE",
                        Ok(PushOutcome::Permanent(_)) => "PERMANENT_FAILURE",
                        Err(_) => "RETRYABLE_FAILURE",
                    }
                } else {
                    "PERMANENT_FAILURE"
                }
            }
        };
        results.push(json!({ "pushTargetRef": id, "outcome": outcome }));
    }
    Ok(json!({ "results": results }))
}

/// Push retention: disabled endpoints are deleted after 30 days.
pub async fn purge<C: ConnectionTrait>(conn: &C) -> Result<u64, DbErr> {
    db::exec(conn, "DELETE FROM notif_push_endpoints WHERE NOT enabled AND updated_at < now() - interval '30 days'", vec![]).await
}

/// How long a PUSH delivery may wait (the transport TTL).
pub fn default_expiry(now: DateTime<Utc>) -> DateTime<Utc> {
    now + Duration::seconds(cf_push_transport::adapters::TTL_SECONDS as i64)
}
