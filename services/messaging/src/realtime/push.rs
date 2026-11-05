//! The mobile push notification boundary (§68).
//!
//! * Devices register a push handle and a preference (enabled / disabled) — per device.
//! * A committed application message creates ONE job per recipient device (idempotent:
//!   unique per event and device) — never for the sender, never for revoked or disabled
//!   devices. Payloads are METADATA ONLY (`v`, `type`, conversation id, event id,
//!   sequence): never plaintext, ciphertext or keys — for PRIVATE_E2EE the client
//!   fetches and decrypts the message itself after waking up. A database CHECK refuses
//!   content-like payload keys.
//! * Jobs are leased (`FOR UPDATE SKIP LOCKED`), retried with bounded backoff and expire.
//!   Without a configured provider they end as `NOT_CONFIGURED` — never reported as
//!   delivered. The APNs, FCM and Web Push adapters live in
//!   [`super::push_adapters`] (each enabled only with its credentials);
//!   [`LocalTestPush`] is an explicitly local adapter for tests and development.
//! * A push is never proof of message delivery.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    conversations::service::rfc3339,
    db,
    error::{Error, ErrorCode, Result},
    events::{bounded_error, outbox::OutboxEvent, RetryPolicy},
    http::middleware::Acting,
    identity::TrustedContext,
    state::AppState,
};

pub const PLATFORMS: &[&str] = &["APNS", "FCM", "WEB_PUSH", "LOCAL_TEST"];

/// Outcome of one provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    Sent,
    Retryable(String),
    /// The provider refused the handle for good (unregistered app, bad token).
    InvalidHandle,
    Permanent(String),
}

#[async_trait::async_trait]
pub trait PushProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Whether this provider can deliver to `platform` in this deployment.
    fn supports(&self, platform: &str) -> bool;
    /// Provider-specific checks of a handle at registration (e.g. the Web Push host
    /// allowlist); structural checks run regardless.
    fn validate_handle(&self, _platform: &str, _handle: &str) -> Result<(), &'static str> {
        Ok(())
    }
    async fn send(&self, platform: &str, handle: &str, payload: &Value) -> PushOutcome;
}

/// No provider configured: nothing is ever reported as sent.
pub struct UnconfiguredPush;

#[async_trait::async_trait]
impl PushProvider for UnconfiguredPush {
    fn name(&self) -> &'static str {
        "unconfigured"
    }

    fn supports(&self, _platform: &str) -> bool {
        false
    }

    async fn send(&self, _platform: &str, _handle: &str, _payload: &Value) -> PushOutcome {
        PushOutcome::Permanent("no push provider configured".into())
    }
}

/// A LOCAL adapter (tests / development only; refused in staging and production by the
/// configuration): records what would have been sent to `LOCAL_TEST` registrations.
#[derive(Default)]
pub struct LocalTestPush {
    pub sent: Mutex<Vec<(String, Value)>>,
    pub fail_next: Mutex<u32>,
}

#[async_trait::async_trait]
impl PushProvider for LocalTestPush {
    fn name(&self) -> &'static str {
        "local-test"
    }

    fn supports(&self, platform: &str) -> bool {
        platform == "LOCAL_TEST"
    }

    async fn send(&self, _platform: &str, handle: &str, payload: &Value) -> PushOutcome {
        let mut fail = self.fail_next.lock().unwrap();
        if *fail > 0 {
            *fail -= 1;
            return PushOutcome::Retryable("injected failure".into());
        }
        self.sent
            .lock()
            .unwrap()
            .push((handle.to_string(), payload.clone()));
        PushOutcome::Sent
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationRequest {
    pub platform: String,
    pub handle: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// `PUT /v1/messaging/devices/{deviceId}/push-registration`.
pub async fn register(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
    request: RegistrationRequest,
) -> Result<Value> {
    if !PLATFORMS.contains(&request.platform.as_str()) {
        return Err(Error::validation(
            "platform must be APNS, FCM, WEB_PUSH or LOCAL_TEST",
        ));
    }
    if !(8..=4096).contains(&request.handle.len()) || request.handle.chars().any(char::is_control) {
        return Err(Error::validation(
            "handle must be 8-4096 printable characters",
        ));
    }
    super::push_adapters::validate_handle_shape(&request.platform, &request.handle)
        .and_then(|_| {
            state
                .realtime
                .push
                .validate_handle(&request.platform, &request.handle)
        })
        .map_err(Error::validation)?;
    crate::messages::e2ee::require_own_device(&state.db, ctx, acting.principal_id, device_id)
        .await?;
    let now = state.now();
    db::exec(
        &state.db,
        "INSERT INTO messaging_push_registrations (id, tenant_id, application_id, environment_id, principal_id, device_id, \
           platform, push_handle, enabled, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10) \
         ON CONFLICT (device_id) DO UPDATE SET platform = EXCLUDED.platform, push_handle = EXCLUDED.push_handle, \
           enabled = EXCLUDED.enabled, disabled_reason = NULL, updated_at = EXCLUDED.updated_at",
        vec![
            Uuid::new_v4().into(),
            ctx.scope.tenant_id.into(),
            ctx.scope.application_id.into(),
            ctx.scope.environment_id.into(),
            acting.principal_id.into(),
            device_id.into(),
            request.platform.clone().into(),
            request.handle.into(),
            request.enabled.into(),
            now.into(),
        ],
    )
    .await?;
    Ok(json!({
        "deviceId": device_id,
        "platform": request.platform,
        "enabled": request.enabled,
        "pushDeliveryConfigured": state.realtime.push.supports(&request.platform),
        "updatedAt": rfc3339(now),
    }))
}

pub async fn unregister(
    state: &AppState,
    ctx: &TrustedContext,
    acting: &Acting,
    device_id: Uuid,
) -> Result<Value> {
    crate::messages::e2ee::require_own_device(&state.db, ctx, acting.principal_id, device_id)
        .await?;
    let n = db::exec(
        &state.db,
        "UPDATE messaging_push_registrations SET enabled = FALSE, disabled_reason = 'UNREGISTERED', updated_at = $2 WHERE device_id = $1",
        vec![device_id.into(), state.now().into()],
    )
    .await?;
    if n == 0 {
        return Err(Error::code(ErrorCode::NotFound));
    }
    Ok(json!({ "deviceId": device_id, "enabled": false }))
}

/// Disables a revoked device's registration (device revocation transaction).
pub async fn disable_device<C: sea_orm::ConnectionTrait>(
    db: &C,
    device_id: Uuid,
    now: DateTime<Utc>,
) -> Result<()> {
    db::exec(
        db,
        "UPDATE messaging_push_registrations SET enabled = FALSE, disabled_reason = 'DEVICE_REVOKED', updated_at = $2 WHERE device_id = $1",
        vec![device_id.into(), now.into()],
    )
    .await?;
    Ok(())
}

/// Creates the push jobs of one committed message event (idempotent).
pub async fn fanout_jobs(state: &AppState, event: &OutboxEvent) -> Result<u64> {
    let (Some(conversation), Some(cursor)) =
        (event.conversation_id, event.conversation_event_sequence)
    else {
        return Ok(0);
    };
    if event.payload.get("notify").and_then(Value::as_bool) == Some(false) {
        return Ok(0);
    }
    let sender = event
        .payload
        .get("sender_principal_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let sequence = event
        .payload
        .get("sequence")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let now = state.now();
    Ok(db::exec(
        &state.db,
        "INSERT INTO messaging_push_jobs (id, event_id, conversation_id, environment_id, device_id, registration_id, payload, \
           next_attempt_at, expires_at, created_at) \
         SELECT gen_random_uuid(), $1, $2, m.environment_id, r.device_id, r.id, \
                jsonb_build_object('v', 1, 'type', 'message', 'conversationId', $2::text, 'eventSequence', $3::bigint, \
                                   'sequence', $4::bigint), $5, $6, $5 \
           FROM messaging_conversation_members m \
           JOIN messaging_push_registrations r ON r.environment_id = m.environment_id AND r.principal_id = m.principal_id AND r.enabled \
           JOIN messaging_devices d ON d.id = r.device_id AND d.status = 'ACTIVE' \
          WHERE m.conversation_id = $2 AND m.state = 'ACTIVE' AND m.principal_id IS DISTINCT FROM $7 \
         ON CONFLICT (event_id, device_id) DO NOTHING",
        vec![
            event.id.into(),
            conversation.into(),
            cursor.into(),
            sequence.into(),
            now.into(),
            (now + chrono::Duration::hours(24)).into(),
            sender.into(),
        ],
    )
    .await?)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PushReport {
    pub claimed: usize,
    pub sent: usize,
    pub retried: usize,
    pub not_configured: usize,
    pub disabled: usize,
    pub failed: usize,
    pub expired: usize,
}

#[derive(FromQueryResult)]
struct Job {
    id: Uuid,
    attempts: i32,
    payload: Value,
    expires_at: DateTime<Utc>,
    platform: String,
    push_handle: String,
    enabled: bool,
    device_status: String,
    recipient_principal_id: Uuid,
    member_active: bool,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    event_payload: Value,
    context_class: String,
    conversation_id: Uuid,
}

/// Reauthorize delayed notifications. The scope comes from the committed owner event,
/// never a push registration or consumer request. Legacy jobs without authority refs
/// are suppressed in the governed personal scope, rather than guessed.
async fn authorized(state: &AppState, job: &Job) -> Result<bool> {
    if !job.member_active {
        return Ok(false);
    }
    let sender = job.event_payload["sender_principal_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok());
    let Some(sender) = sender else {
        return Ok(false);
    };
    if job.context_class == "DIRECT"
        && crate::safety::blocks::blocked_between(
            &state.db,
            job.environment_id,
            sender,
            job.recipient_principal_id,
        )
        .await?
    {
        return Ok(false);
    }
    let Some(client) = state.communications.as_ref() else {
        return Ok(true);
    };
    let scope = cf_communications_policy::Scope {
        tenant_id: job.tenant_id,
        application_id: job.application_id,
        environment_id: job.environment_id,
    };
    if !client.governs(scope) || !matches!(job.context_class.as_str(), "DIRECT" | "GROUP") {
        return Ok(true);
    }
    let Some(account) = job.event_payload["initiating_service_account_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(false);
    };
    let kind = match job.event_payload["environment_kind"].as_str() {
        Some("PRODUCTION") => cf_service_auth::ApplicationEnvironmentKind::Production,
        Some("TEST") => cf_service_auth::ApplicationEnvironmentKind::Test,
        Some("SANDBOX") => cf_service_auth::ApplicationEnvironmentKind::Sandbox,
        _ => return Ok(false),
    };
    let scope = cf_service_auth::DelegatedPlatformContext::new(
        job.tenant_id,
        job.application_id,
        job.environment_id,
        kind,
        account,
    );
    let action = if job.context_class == "DIRECT" {
        cf_communications_policy::Action::MessageNotification
    } else {
        cf_communications_policy::Action::GroupNotification
    };
    match client
        .check(
            scope,
            &cf_communications_policy::PolicyCheck {
                action,
                actor_principal_id: sender,
                target_principal_id: job.recipient_principal_id,
                credential_issued_at: None,
                credential_principal_id: None,
                conversation_id: Some(job.conversation_id),
            },
        )
        .await
    {
        Ok(()) => Ok(true),
        Err(cf_communications_policy::PolicyError::Denied) => Ok(false),
        Err(cf_communications_policy::PolicyError::Unavailable) => {
            Err(Error::code(ErrorCode::ServiceUnavailable))
        }
    }
}

/// One dispatch pass: claims due jobs under a lease and calls the provider.
pub async fn dispatch_due(state: &AppState, batch: i64, max_attempts: i32) -> Result<PushReport> {
    let now = state.now();
    let token = Uuid::new_v4();
    let jobs: Vec<Job> = db::all(
        &state.db,
        "WITH due AS (SELECT id FROM messaging_push_jobs WHERE status = 'PENDING' AND next_attempt_at <= $1 \
             AND (lease_expires_at IS NULL OR lease_expires_at <= $1) ORDER BY next_attempt_at LIMIT $2 FOR UPDATE SKIP LOCKED) \
         UPDATE messaging_push_jobs j SET lease_token = $3, lease_expires_at = $4 FROM due WHERE j.id = due.id \
         RETURNING j.id, j.attempts, j.payload, j.expires_at, j.conversation_id, \
           (SELECT platform FROM messaging_push_registrations r WHERE r.id = j.registration_id) AS platform, \
           (SELECT push_handle FROM messaging_push_registrations r WHERE r.id = j.registration_id) AS push_handle, \
           (SELECT enabled FROM messaging_push_registrations r WHERE r.id = j.registration_id) AS enabled, \
           (SELECT status FROM messaging_devices d WHERE d.id = j.device_id) AS device_status, \
           (SELECT principal_id FROM messaging_devices d WHERE d.id = j.device_id) AS recipient_principal_id, \
           j.environment_id, \
           (SELECT tenant_id FROM messaging_outbox_events e WHERE e.id=j.event_id) AS tenant_id, \
           (SELECT application_id FROM messaging_outbox_events e WHERE e.id=j.event_id) AS application_id, \
           (SELECT payload FROM messaging_outbox_events e WHERE e.id=j.event_id) AS event_payload, \
           (SELECT context_class FROM messaging_conversations c WHERE c.id=j.conversation_id) AS context_class, \
           EXISTS(SELECT 1 FROM messaging_conversation_members m JOIN messaging_devices d ON d.principal_id=m.principal_id AND d.environment_id=m.environment_id \
             JOIN messaging_outbox_events e ON e.id=j.event_id JOIN messaging_conversations c ON c.id=m.conversation_id \
             WHERE m.conversation_id=j.conversation_id AND d.id=j.device_id AND m.state='ACTIVE' \
             AND c.lifecycle_state='ACTIVE' AND e.conversation_event_sequence>=m.visible_from_event) AS member_active",
        vec![now.into(), batch.into(), token.into(), (now + chrono::Duration::seconds(60)).into()],
    )
    .await?;
    let mut report = PushReport {
        claimed: jobs.len(),
        ..Default::default()
    };
    let retry = RetryPolicy::default();
    for job in jobs {
        let attempts = job.attempts + 1;
        let (status, next, error): (&str, Option<DateTime<Utc>>, Option<String>) = if !job.enabled
            || job.device_status != "ACTIVE"
        {
            report.disabled += 1;
            ("DISABLED", None, None)
        } else if job.expires_at <= now {
            report.expired += 1;
            ("EXPIRED", None, None)
        } else if !state.realtime.push.supports(&job.platform) {
            report.not_configured += 1;
            ("NOT_CONFIGURED", None, None)
        } else {
            let outcome = match authorized(state, &job).await {
                Ok(true) => {
                    state
                        .realtime
                        .push
                        .send(&job.platform, &job.push_handle, &job.payload)
                        .await
                }
                Ok(false) => {
                    report.disabled += 1;
                    db::exec(&state.db,
                        "UPDATE messaging_push_jobs SET status='DISABLED',attempts=$3,lease_token=NULL,lease_expires_at=NULL,completed_at=$4 WHERE id=$1 AND lease_token=$2",
                        vec![job.id.into(),token.into(),attempts.into(),now.into()]).await?;
                    crate::metrics::inc("messaging_push_jobs_total", &[("status", "disabled")]);
                    continue;
                }
                Err(_) => PushOutcome::Retryable("notification authorization unavailable".into()),
            };
            match outcome {
                PushOutcome::Sent => {
                    report.sent += 1;
                    ("SENT", None, None)
                }
                PushOutcome::InvalidHandle => {
                    report.failed += 1;
                    let _ = db::exec(
                        &state.db,
                        "UPDATE messaging_push_registrations SET enabled = FALSE, disabled_reason = 'INVALID_HANDLE', updated_at = $2 \
                         WHERE id = (SELECT registration_id FROM messaging_push_jobs WHERE id = $1) AND push_handle=$3",
                        vec![job.id.into(), now.into(), job.push_handle.into()],
                    )
                    .await;
                    ("FAILED", None, Some("invalid handle".into()))
                }
                PushOutcome::Permanent(e) => {
                    report.failed += 1;
                    ("FAILED", None, Some(e))
                }
                PushOutcome::Retryable(e) if attempts >= max_attempts => {
                    report.failed += 1;
                    ("FAILED", None, Some(e))
                }
                PushOutcome::Retryable(e) => {
                    report.retried += 1;
                    ("PENDING", Some(now + retry.delay(attempts)), Some(e))
                }
            }
        };
        db::exec(
            &state.db,
            "UPDATE messaging_push_jobs SET status = $3, attempts = $4, next_attempt_at = COALESCE($5, next_attempt_at), \
               last_error = $6, lease_token = NULL, lease_expires_at = NULL, \
               completed_at = CASE WHEN $3 = 'PENDING' THEN NULL ELSE $7 END \
             WHERE id = $1 AND lease_token = $2",
            vec![
                job.id.into(),
                token.into(),
                status.into(),
                attempts.into(),
                next.into(),
                error.map(|e| bounded_error(&e)).into(),
                now.into(),
            ],
        )
        .await?;
        crate::metrics::inc(
            "messaging_push_jobs_total",
            &[("status", status_label(status))],
        );
    }
    Ok(report)
}

fn status_label(s: &str) -> &'static str {
    match s {
        "SENT" => "sent",
        "PENDING" => "retry",
        "DISABLED" => "disabled",
        "EXPIRED" => "expired",
        "NOT_CONFIGURED" => "not_configured",
        _ => "failed",
    }
}

pub async fn run(state: AppState, interval: std::time::Duration) {
    loop {
        if let Err(e) = dispatch_due(&state, 200, 8).await {
            tracing::warn!(error = %e, "push dispatch pass failed");
        }
        tokio::time::sleep(interval).await;
    }
}
