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
//!   The lease token is the fence: immediately before each authorization check and each
//!   provider call the job's lease is renewed under its token, and a job whose lease was
//!   re-claimed by another dispatcher is skipped, never sent twice (S-8). Each of those
//!   calls is bounded by a third of the lease.
//!   Without a configured provider they end as `NOT_CONFIGURED` — never reported as
//!   delivered. The APNs, FCM and Web Push adapters live in
//!   [`super::push_adapters`] (each enabled only with its credentials);
//!   [`LocalTestPush`] is an explicitly local adapter for tests and development.
//! * Who may be woken is decided in ONE place, [`authorize_wakeups`]: job creation takes
//!   its candidates, dispatch rechecks its entitlement for the job's recipient, and the
//!   Notification Service asks the same question through
//!   `POST /internal/v1/notifications/wakeups/authorize` ([`crate::notifications`]).
//! * A push is never proof of message delivery.

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

// The provider boundary (`PushOutcome`, `PushProvider`) and the local test adapter moved to
// packages/push-transport with the adapters (Notification Service roadmap N4); re-exported
// here under their previous paths so every caller and test is unchanged.
pub use cf_push_transport::provider::{LocalTestPush, PushOutcome, PushProvider};

/// `GET /v1/messaging/push/configuration`: which push platforms this deployment delivers
/// to, and the Web Push application-server key. Nothing secret, nothing per user.
pub fn configuration(state: &crate::state::AppState) -> Value {
    let push = &state.realtime.push;
    serde_json::json!({
        "platforms": {
            "APNS": push.supports("APNS"),
            "FCM": push.supports("FCM"),
            "WEB_PUSH": push.supports("WEB_PUSH"),
        },
        "webPush": push
            .web_push_application_server_key()
            .map(|key| serde_json::json!({ "applicationServerKey": key })),
        "payload": "METADATA_ONLY",
    })
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
    // N9 facade (off by default): mirror the committed registration to the service.
    crate::push_mirror::after_change(state, device_id, Some(ctx.scope));
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
    crate::push_mirror::after_change(state, device_id, Some(ctx.scope));
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

/// One principal [`authorize_wakeups`] entitles to a wake-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntitledRecipient {
    pub principal_id: Uuid,
    /// MANAGED only: the event's message mentions this principal
    /// (`messaging_message_mentions`). Always `false` for PRIVATE_E2EE (mentions stay
    /// inside the ciphertext).
    pub mentions_recipient: bool,
}

/// The wake-up decision for one committed conversation event: the single place the push
/// entitlement rules live (Notification Service X3, Document 4 §3.4). Push job creation
/// ([`fanout_jobs`]), dispatch-time authorization (`authorized`) and the owner API
/// `POST /internal/v1/notifications/wakeups/authorize` ([`crate::notifications`]) all
/// read it. Ids, classes and states only: never content, ciphertext or keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeupAuthorization {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    /// `lifecycle_state = 'ACTIVE'`.
    pub conversation_active: bool,
    /// `PRIVATE_E2EE` or `MANAGED`.
    pub security_profile: String,
    /// The conversation's context class (`DIRECT`, `GROUP`, ...).
    pub context_class: String,
    /// The sender the committed event records (`sender_principal_id`).
    pub sender: Option<Uuid>,
    /// The event's non-secret authority references (as for delayed policy checks).
    pub initiating_service_account_id: Option<Uuid>,
    pub environment_kind: Option<String>,
    /// Push job candidates: ACTIVE members other than the sender (empty when the event
    /// wakes nobody). Jobs are created for these at commit; delivery is gated by
    /// `entitled` again at dispatch, so a later departure or block still suppresses it.
    pub candidates: Vec<Uuid>,
    /// The principals entitled to a wake-up now: candidates whose history window covers
    /// the event, in an ACTIVE conversation, with a recorded sender and, for DIRECT, no
    /// block between sender and recipient. Ordered by principal id.
    pub entitled: Vec<EntitledRecipient>,
}

impl WakeupAuthorization {
    pub fn entitles(&self, principal: Uuid) -> bool {
        self.entitled.iter().any(|e| e.principal_id == principal)
    }
}

/// Whether a committed event wakes recipients at all: a created APPLICATION message not
/// marked `notify: false` (mirrored, migrated and imported messages).
pub fn wakes(event_type: &str, payload: &Value) -> bool {
    event_type == "messaging.message.created"
        && payload.get("message_kind").and_then(Value::as_str) == Some("APPLICATION")
        && payload.get("notify").and_then(Value::as_bool) != Some(false)
}

#[derive(FromQueryResult)]
struct WakeupEventRow {
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    security_profile: String,
    context_class: String,
    lifecycle_state: String,
    event_type: Option<String>,
    payload: Option<Value>,
}

#[derive(FromQueryResult)]
struct CandidateRow {
    principal_id: Uuid,
    in_window: bool,
}

#[derive(FromQueryResult)]
struct MentionRow {
    principal_id: Uuid,
}

/// The wake-up decision of the event at `(conversation_id, event_sequence)` (the
/// conversation event cursor: `eventSequence` of the push payload). `None`: no such
/// conversation, or no such event in it. `only` restricts the answer to those principals
/// (dispatch-time recheck). Read-only.
pub async fn authorize_wakeups<C: sea_orm::ConnectionTrait>(
    db: &C,
    conversation_id: Uuid,
    event_sequence: i64,
    only: Option<&[Uuid]>,
) -> Result<Option<WakeupAuthorization>> {
    let Some(row) = db::one::<WakeupEventRow, _>(
        db,
        "SELECT c.tenant_id, c.application_id, c.environment_id, c.security_profile, c.context_class, \
                c.lifecycle_state, e.event_type, e.payload \
           FROM messaging_conversations c \
           LEFT JOIN messaging_outbox_events e ON e.conversation_id = c.id AND e.conversation_event_sequence = $2 \
          WHERE c.id = $1",
        vec![conversation_id.into(), event_sequence.into()],
    )
    .await?
    else {
        return Ok(None);
    };
    let (Some(event_type), Some(payload)) = (row.event_type, row.payload) else {
        return Ok(None);
    };
    let uuid_of = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let sender = uuid_of("sender_principal_id");
    let mut decision = WakeupAuthorization {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
        conversation_active: row.lifecycle_state == "ACTIVE",
        security_profile: row.security_profile,
        context_class: row.context_class,
        sender,
        initiating_service_account_id: uuid_of("initiating_service_account_id"),
        environment_kind: payload
            .get("environment_kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        candidates: Vec::new(),
        entitled: Vec::new(),
    };
    if !wakes(&event_type, &payload) || only.is_some_and(<[Uuid]>::is_empty) {
        return Ok(Some(decision));
    }
    let mut values = vec![conversation_id.into(), event_sequence.into(), sender.into()];
    let restriction = match only {
        Some(principals) => {
            values.push(principals.to_vec().into());
            " AND m.principal_id = ANY($4)"
        }
        None => "",
    };
    let candidates: Vec<CandidateRow> = db::all(
        db,
        &format!(
            "SELECT m.principal_id, ($2 >= m.visible_from_event) AS in_window \
               FROM messaging_conversation_members m \
              WHERE m.conversation_id = $1 AND m.state = 'ACTIVE' AND m.principal_id IS DISTINCT FROM $3{restriction} \
              ORDER BY m.principal_id"
        ),
        values,
    )
    .await?;
    decision.candidates = candidates.iter().map(|c| c.principal_id).collect();
    let Some(sender) = sender.filter(|_| decision.conversation_active) else {
        return Ok(Some(decision));
    };
    let mentioned: Vec<Uuid> = match uuid_of("message_id") {
        Some(message) if decision.security_profile == "MANAGED" => db::all::<MentionRow, _>(
            db,
            "SELECT principal_id FROM messaging_message_mentions WHERE message_id = $1",
            vec![message.into()],
        )
        .await?
        .into_iter()
        .map(|m| m.principal_id)
        .collect(),
        _ => Vec::new(),
    };
    for candidate in candidates.into_iter().filter(|c| c.in_window) {
        if decision.context_class == "DIRECT"
            && crate::safety::blocks::blocked_between(
                db,
                decision.environment_id,
                sender,
                candidate.principal_id,
            )
            .await?
        {
            continue;
        }
        decision.entitled.push(EntitledRecipient {
            principal_id: candidate.principal_id,
            mentions_recipient: mentioned.contains(&candidate.principal_id),
        });
    }
    Ok(Some(decision))
}

/// The legacy push job insert of one event (idempotent per event and device).
const INSERT_JOBS: &str = "INSERT INTO messaging_push_jobs (id, event_id, conversation_id, environment_id, device_id, registration_id, payload, \
       next_attempt_at, expires_at, created_at) \
     SELECT gen_random_uuid(), $1, $2, m.environment_id, r.device_id, r.id, \
            jsonb_build_object('v', 1, 'type', 'message', 'conversationId', $2::text, 'eventSequence', $3::bigint, \
                               'sequence', $4::bigint), $5, $6, $5 \
       FROM messaging_conversation_members m \
       JOIN messaging_push_registrations r ON r.environment_id = m.environment_id AND r.principal_id = m.principal_id AND r.enabled \
       JOIN messaging_devices d ON d.id = r.device_id AND d.status = 'ACTIVE' \
      WHERE m.conversation_id = $2 AND m.state = 'ACTIVE' AND m.principal_id = ANY($7) \
     ON CONFLICT (event_id, device_id) DO NOTHING";

/// Creates the push jobs of one committed message event (idempotent): one per enabled
/// registration of an ACTIVE device of each [`WakeupAuthorization::candidates`]
/// principal. Delivery is authorized again at dispatch.
///
/// Routing fence (Document 4 §9.3; [`crate::notification_fence`]): the mode of
/// `MESSAGE_WAKEUPS` is read first. `LEGACY_ONLY` (or no fence): the jobs are written
/// exactly as before, without a decision. Otherwise ONE transaction records the event's
/// routing decision (key [`fence::push_key`], sticky) and writes what it routes to:
/// * `OLD` — the push jobs; in `SHADOW` a newly made decision also enqueues one shadow
///   publication ([`crate::notification_publications`]) the service never delivers;
/// * `NEW` (`FENCED_NEW`, inside the canary) — no push jobs, one publication.
pub async fn fanout_jobs(state: &AppState, event: &OutboxEvent) -> Result<u64> {
    use crate::notification_fence::{self as fence, Mode, Route};
    use sea_orm::TransactionTrait;

    let (Some(conversation), Some(cursor)) =
        (event.conversation_id, event.conversation_event_sequence)
    else {
        return Ok(0);
    };
    let Some(decision) = authorize_wakeups(&state.db, conversation, cursor, None).await? else {
        return Ok(0);
    };
    if decision.candidates.is_empty() {
        return Ok(0);
    }
    let sequence = event
        .payload
        .get("sequence")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let now = state.now();
    let expires_at = now + chrono::Duration::hours(24);
    let new_capable =
        decision.candidates.len() <= crate::notification_publications::NEW_ROUTE_MAX_CANDIDATES;
    let values = vec![
        event.id.into(),
        conversation.into(),
        cursor.into(),
        sequence.into(),
        now.into(),
        expires_at.into(),
        decision.candidates.into(),
    ];
    match fence::current_mode(&state.db, fence::MESSAGE_WAKEUPS).await? {
        None | Some(Mode::LegacyOnly) => return Ok(db::exec(&state.db, INSERT_JOBS, values).await?),
        Some(_) => {}
    }
    let txn = state.db.begin().await?;
    let route = fence::route_message(
        &txn,
        &fence::push_key(event.id),
        fence::MESSAGE_WAKEUPS,
        expires_at,
        new_capable,
    )
    .await?;
    let jobs = match route.route {
        Route::Old => {
            let jobs = db::exec(&txn, INSERT_JOBS, values).await?;
            if route.created && route.mode == Some(Mode::Shadow) {
                crate::notification_publications::enqueue(
                    &txn,
                    event,
                    conversation,
                    cursor,
                    &route,
                    true,
                    now,
                )
                .await?;
            }
            jobs
        }
        Route::New => {
            crate::notification_publications::enqueue(
                &txn,
                event,
                conversation,
                cursor,
                &route,
                false,
                now,
            )
            .await?;
            0
        }
    };
    txn.commit().await?;
    Ok(jobs)
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
    /// Claimed jobs skipped because their lease lapsed and another dispatcher re-claimed
    /// them (that dispatcher sends them).
    pub lease_lost: usize,
}

/// How long a dispatcher holds a claimed job. Renewed under the lease token before each
/// external call of the job, so a slow provider earlier in a batch cannot let a later
/// job's lease lapse into another replica's claim while this one still sends it.
pub const LEASE: chrono::Duration = chrono::Duration::seconds(60);

/// Upper bound of one authorization check or one provider call: a third of [`LEASE`], so
/// the lease renewed just before the call always outlives it. (The adapters' own HTTP
/// timeouts are shorter; this only bounds a call that hangs.)
const CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Extends the lease of a job this dispatcher still holds. `false`: the lease token no
/// longer matches (the lease lapsed and another dispatcher re-claimed the job, or the job
/// is no longer pending) — the caller must not send it.
async fn renew_lease(state: &AppState, job: Uuid, token: Uuid) -> Result<bool> {
    let renewed = db::exec(
        &state.db,
        "UPDATE messaging_push_jobs SET lease_expires_at = $3 WHERE id = $1 AND lease_token = $2 AND status = 'PENDING'",
        vec![job.into(), token.into(), (state.now() + LEASE).into()],
    )
    .await?;
    Ok(renewed == 1)
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
    event_sequence: Option<i64>,
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
    // The shared wake-up decision (ACTIVE member, history window, conversation ACTIVE,
    // a recorded sender, DIRECT block check), rechecked now for this recipient.
    let Some(sequence) = job.event_sequence else {
        return Ok(false);
    };
    let recipient = [job.recipient_principal_id];
    let decision =
        authorize_wakeups(&state.db, job.conversation_id, sequence, Some(&recipient)).await?;
    let Some(sender) = decision
        .filter(|d| d.entitles(job.recipient_principal_id))
        .and_then(|d| d.sender)
    else {
        return Ok(false);
    };
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
    // Gap audit S10: a recipient mentioned in a MANAGED message passes a "mentions only"
    // preference (mentions are recorded only for MANAGED text, never inferred).
    let mentioned = match job.event_payload["message_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(message) => {
            db::scalar_i64(
                &state.db,
                "SELECT count(*) FROM messaging_message_mentions WHERE message_id = $1 AND principal_id = $2",
                vec![message.into(), job.recipient_principal_id.into()],
            )
            .await?
                > 0
        }
        None => false,
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
                mentioned: mentioned.then_some(true),
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
           (SELECT conversation_event_sequence FROM messaging_outbox_events e WHERE e.id=j.event_id) AS event_sequence",
        vec![now.into(), batch.into(), token.into(), (now + LEASE).into()],
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
            if !renew_lease(state, job.id, token).await? {
                lease_lost(&mut report);
                continue;
            }
            let verdict = tokio::time::timeout(CALL_DEADLINE, authorized(state, &job))
                .await
                .unwrap_or_else(|_| Err(Error::code(ErrorCode::ServiceUnavailable)));
            let outcome = match verdict {
                Ok(true) => {
                    // The fence: the send happens only while this dispatcher provably
                    // holds the job, for at least the whole (bounded) call.
                    if !renew_lease(state, job.id, token).await? {
                        lease_lost(&mut report);
                        continue;
                    }
                    let send =
                        state
                            .realtime
                            .push
                            .send(&job.platform, &job.push_handle, &job.payload);
                    tokio::time::timeout(CALL_DEADLINE, send)
                        .await
                        .unwrap_or_else(|_| {
                            PushOutcome::Retryable("provider call timed out".into())
                        })
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

/// How long a push job in a final state (`SENT`, `FAILED`, `EXPIRED`, `DISABLED`,
/// `NOT_CONFIGURED`) is kept after `completed_at` (delivery diagnostics, the owner's push
/// status view) before [`purge_completed`] removes it. Pending jobs are never purged.
pub const JOB_RETENTION: chrono::Duration = chrono::Duration::days(30);

/// Most jobs one [`purge_completed`] pass removes.
pub const PURGE_BATCH: i64 = 5_000;

/// One bounded retention pass: deletes at most `batch` final-state jobs completed more
/// than [`JOB_RETENTION`] ago, oldest first. Idempotent and safe on several instances.
/// Run hourly by the maintenance worker; deliberately without a new index (no migration,
/// so the previous image still passes readiness after a rollback; the table is small until
/// push moves to the Notification Service in N9). Returns the number of jobs deleted.
pub async fn purge_completed(state: &AppState, batch: i64) -> Result<u64> {
    let cutoff = state.now() - JOB_RETENTION;
    Ok(db::exec(
        &state.db,
        "DELETE FROM messaging_push_jobs WHERE id IN (SELECT id FROM messaging_push_jobs WHERE status <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
        vec![cutoff.into(), batch.into()],
    )
    .await?)
}

fn lease_lost(report: &mut PushReport) {
    report.lease_lost += 1;
    crate::metrics::inc("messaging_push_lease_lost_total", &[]);
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
