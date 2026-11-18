//! Partner push (ADR 0063; build contract §4 and §6 N-P2 / N-P4), behind
//! `MESSAGING_PARTNER_PUSH_ENABLED` (default off).
//!
//! * **Registrations (N-P2).** `PUT` / `DELETE /v1/messaging/user-push-registrations/
//!   {installationId}` (user token, `msg` grant) are synchronised to the Notification
//!   Service's existing registration sync (I13, `notifications-push-registration.sync`)
//!   with the subject `{kind: "platform-principal", tenantId, applicationId,
//!   environmentId, principalId, installationId}`. The handle travels only in that
//!   authenticated body: Messaging keeps the registration's identity and status, never
//!   the handle. The first-party mirror ([`crate::push_mirror`]) is untouched.
//! * **Wake-ups (N-P4).** For a partner MANAGED message whose conversation has members
//!   with an ACTIVE partner registration, the realtime fan-out records one publication
//!   (one row per outbox event); the relay ([`run`]) posts its metadata-only
//!   `messaging.message.created` envelope to I1 with the recipients
//!   `platform-principal:<environmentId>:<principalId>` (the entitled members — mutes,
//!   blocks and history windows as for first-party wake-ups — that registered). Ids only:
//!   never text, sender name or attachment metadata.
//!
//! `packages/notification-client` validates `platform-principal:<uuid>` recipients only
//! (it predates the contract's two-id form): the envelope is built with the client's
//! builder and its recipient references are then written in the contract's form.

use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::{envelope::Scope, EnvelopeBuilder, Producer, RouteRef};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::FromQueryResult;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{sessions, valid_installation_id};
use crate::{
    db,
    error::{Error, ErrorCode, Result},
    events::outbox::OutboxEvent,
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
    push_mirror::{SyncFailure, SYNC_PATH},
    state::AppState,
};

/// Partner push platforms (§4).
pub const PLATFORMS: &[&str] = &["APNS", "APNS_VOIP", "FCM"];
/// The subject kind of a partner registration (N-P2) and the recipient prefix (N-P4).
pub const PLATFORM_PRINCIPAL: &str = "platform-principal";
/// The catalogue type of a message wake-up.
pub const EVENT_TYPE: &str = "messaging.message.created";
/// Largest handle.
pub const MAX_HANDLE: usize = 4096;
/// How long a partner wake-up may wait for the relay.
pub const LIFETIME: Duration = Duration::hours(1);
/// Most wake-ups in one I1 batch.
pub const BATCH: usize = 100;
/// Most recipients of one wake-up (the envelope's limit is larger; ids stay small).
pub const MAX_RECIPIENTS: usize = 40;
/// Counter of relay outcomes.
pub const METRIC: &str = "messaging_partner_push_publications_total";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationRequest {
    pub platform: String,
    pub handle: String,
}

/// One per-item answer of the registration sync (lenient: the partner subject's result
/// names the installation, not a Messaging device).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationResult {
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Where partner registrations go (N-P2).
#[async_trait]
pub trait PartnerRegistrationSink: Send + Sync {
    async fn sync(
        &self,
        registrations: &[Value],
    ) -> std::result::Result<Vec<RegistrationResult>, SyncFailure>;
}

/// The Notification Service's registration sync (I13).
pub struct NotificationServicePartnerRegistrations {
    client: Arc<InternalServiceClient>,
}

impl NotificationServicePartnerRegistrations {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct SyncAnswer {
    results: Vec<RegistrationResult>,
}

#[async_trait]
impl PartnerRegistrationSink for NotificationServicePartnerRegistrations {
    async fn sync(
        &self,
        registrations: &[Value],
    ) -> std::result::Result<Vec<RegistrationResult>, SyncFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        match self
            .client
            .exchange_json(
                &request,
                Method::POST,
                SYNC_PATH,
                Some(&json!({ "registrations": registrations })),
            )
            .await
        {
            Ok(r) if r.status() == 200 => r
                .json::<SyncAnswer>()
                .map(|a| a.results)
                .map_err(|_| SyncFailure::Unavailable),
            Ok(r) if r.status() >= 500 || matches!(r.status(), 401 | 403 | 408 | 429) => {
                Err(SyncFailure::Unavailable)
            }
            Ok(_) => Err(SyncFailure::Rejected),
            Err(InternalCallError::UnknownTarget(_)) => Err(SyncFailure::NotConfigured),
            Err(_) => Err(SyncFailure::Unavailable),
        }
    }
}

/// No Notification Service configured.
pub struct UnconfiguredRegistrations;

#[async_trait]
impl PartnerRegistrationSink for UnconfiguredRegistrations {
    async fn sync(&self, _: &[Value]) -> std::result::Result<Vec<RegistrationResult>, SyncFailure> {
        Err(SyncFailure::NotConfigured)
    }
}

/// No Notification Service configured (wake-ups wait).
pub struct UnconfiguredWakeups;

#[async_trait]
impl NotificationEventSink for UnconfiguredWakeups {
    async fn ingest(
        &self,
        _: &[Value],
        _: bool,
    ) -> std::result::Result<Vec<IngestResult>, PublishFailure> {
        Err(PublishFailure::NotConfigured)
    }
}

/// The N-P2 subject of a partner registration.
pub fn subject(
    scope: &crate::identity::OwnerScope,
    principal_id: Uuid,
    installation_id: &str,
) -> Value {
    json!({
        "kind": PLATFORM_PRINCIPAL,
        "tenantId": scope.tenant_id,
        "applicationId": scope.application_id,
        "environmentId": scope.environment_id,
        "principalId": principal_id,
        "installationId": installation_id,
    })
}

fn sync_error(f: SyncFailure) -> Error {
    match f {
        SyncFailure::NotConfigured => Error::reason(
            ErrorCode::FeatureDisabled,
            "NOTIFICATIONS_NOT_CONFIGURED",
        ),
        SyncFailure::Unavailable => Error::code(ErrorCode::ServiceUnavailable),
        SyncFailure::Rejected => {
            Error::reason(ErrorCode::ValidationError, "PUSH_REGISTRATION_REFUSED")
        }
    }
}

fn valid_platform(p: &str) -> bool {
    PLATFORMS.contains(&p)
}

async fn caller(state: &AppState, token: &str) -> Result<sessions::PartnerCaller> {
    let caller = sessions::authenticate(state, token).await?;
    if caller.token.grants.msg.is_empty() {
        return Err(Error::reason(ErrorCode::InsufficientScope, "MSG_GRANT_REQUIRED"));
    }
    Ok(caller)
}

/// `PUT /v1/messaging/user-push-registrations/{installationId}`.
pub async fn register(
    state: &AppState,
    token: &str,
    installation_id: &str,
    request: RegistrationRequest,
) -> Result<Value> {
    if !valid_installation_id(installation_id) {
        return Err(Error::validation(
            "installationId must be 8-128 characters of [A-Za-z0-9._-]",
        ));
    }
    if !valid_platform(&request.platform) {
        return Err(Error::validation("platform must be APNS, APNS_VOIP or FCM"));
    }
    if request.handle.is_empty()
        || request.handle.len() > MAX_HANDLE
        || request.handle.chars().any(char::is_control)
    {
        return Err(Error::validation("handle must be 1-4096 printable characters"));
    }
    let caller = caller(state, token).await?;
    let principal = caller.token.user.id;
    let now = state.now();
    let key = format!(
        "partner-push:{}:{}",
        caller.scope.environment_id.simple(),
        principal.simple()
    );
    if let Some(retry) = crate::limits::hit(&state.db, &key, crate::limits::Window::Hour, 60, now).await? {
        return Err(Error::RateLimited {
            retry_after_seconds: retry,
        });
    }
    let item = json!({
        "subject": subject(&caller.scope, principal, installation_id),
        "platform": request.platform,
        "handle": request.handle,
        "enabled": true,
    });
    let results = state
        .partner
        .registrations
        .sync(std::slice::from_ref(&item))
        .await
        .map_err(sync_error)?;
    drop(item);
    let outcome = results.first().map(|r| r.outcome.as_str()).unwrap_or("");
    crate::metrics::inc(
        "messaging_partner_push_registrations_total",
        &[("outcome", if matches!(outcome, "CREATED" | "UPDATED" | "UNCHANGED") { "accepted" } else { "refused" })],
    );
    if !matches!(outcome, "CREATED" | "UPDATED" | "UNCHANGED") {
        return Err(Error::reason(
            ErrorCode::ValidationError,
            "PUSH_REGISTRATION_REFUSED",
        ));
    }
    db::exec(
        &state.db,
        "INSERT INTO messaging_partner_push_registrations (id, tenant_id, application_id, environment_id, principal_id, \
           installation_id, platform, status, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'ACTIVE', $8, $8) \
         ON CONFLICT (environment_id, principal_id, installation_id, platform) \
         DO UPDATE SET status = 'ACTIVE', updated_at = EXCLUDED.updated_at",
        vec![
            Uuid::new_v4().into(),
            caller.scope.tenant_id.into(),
            caller.scope.application_id.into(),
            caller.scope.environment_id.into(),
            principal.into(),
            installation_id.into(),
            request.platform.clone().into(),
            now.into(),
        ],
    )
    .await?;
    Ok(json!({
        "installationId": installation_id,
        "platform": request.platform,
        "status": "ACTIVE",
    }))
}

/// `DELETE /v1/messaging/user-push-registrations/{installationId}?platform=` (every
/// platform of the installation without `platform`). Idempotent.
pub async fn unregister(
    state: &AppState,
    token: &str,
    installation_id: &str,
    platform: Option<&str>,
) -> Result<()> {
    if !valid_installation_id(installation_id) {
        return Err(Error::validation(
            "installationId must be 8-128 characters of [A-Za-z0-9._-]",
        ));
    }
    if platform.is_some_and(|p| !valid_platform(p)) {
        return Err(Error::validation("platform must be APNS, APNS_VOIP or FCM"));
    }
    let caller = caller(state, token).await?;
    let principal = caller.token.user.id;
    let platforms: Vec<&str> = match platform {
        Some(p) => vec![p],
        None => PLATFORMS.to_vec(),
    };
    let items: Vec<Value> = platforms
        .iter()
        .map(|p| {
            json!({
                "subject": subject(&caller.scope, principal, installation_id),
                "platform": p,
                "enabled": false,
            })
        })
        .collect();
    state
        .partner
        .registrations
        .sync(&items)
        .await
        .map_err(sync_error)?;
    db::exec(
        &state.db,
        "UPDATE messaging_partner_push_registrations SET status = 'REMOVED', updated_at = $5 \
         WHERE environment_id = $1 AND principal_id = $2 AND installation_id = $3 AND platform = ANY($4) \
           AND status = 'ACTIVE'",
        vec![
            caller.scope.environment_id.into(),
            principal.into(),
            installation_id.into(),
            platforms
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .into(),
            state.now().into(),
        ],
    )
    .await?;
    Ok(())
}

// ------------------------------------------------------------------------- wake-ups

/// Records the partner wake-up of a committed `messaging.message.created` event (called
/// by the realtime fan-out; idempotent per event). Only MANAGED conversations with an
/// ACTIVE member (other than the sender) holding an ACTIVE partner registration.
pub async fn enqueue_wakeup(state: &AppState, event: &OutboxEvent) -> Result<bool> {
    let (Some(conversation), Some(cursor)) =
        (event.conversation_id, event.conversation_event_sequence)
    else {
        return Ok(false);
    };
    if event.payload.get("message_kind").and_then(Value::as_str) != Some("APPLICATION")
        || event.payload.get("notify").and_then(Value::as_bool) == Some(false)
    {
        return Ok(false);
    }
    let sender = event
        .payload
        .get("sender_principal_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let now = state.now();
    let correlation = cf_service_auth::CorrelationId::parse(&event.correlation_id)
        .map(|c| c.as_str().to_owned())
        .unwrap_or_else(|_| format!("corr_{}", event.id.simple()));
    let written = db::exec(
        &state.db,
        "INSERT INTO messaging_partner_push_publications (id, outbox_event_id, tenant_id, application_id, environment_id, \
           conversation_id, event_sequence, sender_principal_id, correlation_id, next_attempt_at, occurred_at, created_at, \
           expires_at) \
         SELECT $1, $2, c.tenant_id, c.application_id, c.environment_id, c.id, $4, $5, $6, $7, $8, $7, $9 \
           FROM messaging_conversations c \
          WHERE c.id = $3 AND c.security_profile = 'MANAGED' \
            AND EXISTS (SELECT 1 FROM messaging_partner_push_registrations r \
                          JOIN messaging_conversation_members m ON m.principal_id = r.principal_id \
                           AND m.conversation_id = c.id AND m.state = 'ACTIVE' \
                         WHERE r.environment_id = c.environment_id AND r.status = 'ACTIVE' \
                           AND r.principal_id IS DISTINCT FROM $5) \
         ON CONFLICT (outbox_event_id) DO NOTHING",
        vec![
            Uuid::new_v4().into(),
            event.id.into(),
            conversation.into(),
            cursor.into(),
            sender.into(),
            correlation.into(),
            now.into(),
            event.occurred_at.into(),
            (now + LIFETIME).into(),
        ],
    )
    .await?;
    Ok(written > 0)
}

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    outbox_event_id: Uuid,
    tenant_id: Uuid,
    application_id: Uuid,
    environment_id: Uuid,
    conversation_id: Uuid,
    event_sequence: i64,
    correlation_id: String,
    envelope: Option<Value>,
    attempts: i32,
    occurred_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(FromQueryResult)]
struct Principal {
    principal_id: Uuid,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The event id of a partner wake-up (stable per outbox event).
fn event_id(row: &Row) -> String {
    format!("evt_{}", row.outbox_event_id.simple())
}

/// The metadata-only envelope of a row, or why there is nothing to publish.
async fn build(state: &AppState, row: &Row) -> std::result::Result<Value, &'static str> {
    let authorization = crate::realtime::push::authorize_wakeups(
        &state.db,
        row.conversation_id,
        row.event_sequence,
        None,
    )
    .await
    .map_err(|_| "UNAVAILABLE")?
    .ok_or("EVENT_NOT_FOUND")?;
    if !authorization.conversation_active
        || authorization.entitled.is_empty()
        || authorization.security_profile != "MANAGED"
    {
        return Err("NO_ENTITLED_RECIPIENTS");
    }
    let entitled: Vec<Uuid> = authorization
        .entitled
        .iter()
        .map(|e| e.principal_id)
        .collect();
    let registered: Vec<Principal> = db::all(
        &state.db,
        "SELECT DISTINCT principal_id FROM messaging_partner_push_registrations \
          WHERE environment_id = $1 AND status = 'ACTIVE' AND principal_id = ANY($2) ORDER BY principal_id LIMIT $3",
        vec![
            row.environment_id.into(),
            entitled.into(),
            (MAX_RECIPIENTS as i64).into(),
        ],
    )
    .await
    .map_err(|_| "UNAVAILABLE")?;
    if registered.is_empty() {
        return Err("NO_REGISTERED_RECIPIENTS");
    }
    let route = RouteRef {
        decision_id: row.id,
        routing_version: 1,
        protocol: crate::notification_fence::ROUTING_PROTOCOL,
    };
    let mut builder = EnvelopeBuilder::new(
        Producer::Messaging,
        EVENT_TYPE,
        &rfc3339(row.occurred_at),
        route,
    )
    .map_err(|_| "INVALID")?
    .event_id(event_id(row))
    .correlation_id(row.correlation_id.clone())
    .scope(Scope {
        tenant_id: row.tenant_id,
        application_id: row.application_id,
        environment_id: row.environment_id,
    })
    .subject(row.conversation_id, None)
    .dedupe_key(format!("{EVENT_TYPE}:{}", row.outbox_event_id))
    .idempotency_key(event_id(row))
    .coalesce_key(format!("conversation:{}", row.conversation_id))
    .expires_at(rfc3339(row.expires_at))
    .data("conversationId", row.conversation_id.to_string())
    .data(
        "conversationKind",
        crate::notifications::conversation_kind(&authorization.context_class).to_owned(),
    )
    .data("eventSequence", row.event_sequence)
    .data("securityProfile", authorization.security_profile.clone());
    // Validated by the client in its `platform-principal:<id>` form, then written in the
    // contract's `platform-principal:<environmentId>:<principalId>` form.
    for p in &registered {
        builder = builder.recipient(format!("{PLATFORM_PRINCIPAL}:{}", p.principal_id));
    }
    let envelope = builder.build().map_err(|_| "INVALID")?;
    let mut value = serde_json::to_value(envelope).map_err(|_| "INVALID")?;
    value["recipients"] = Value::Array(
        registered
            .iter()
            .map(|p| json!({ "ref": recipient(row.environment_id, p.principal_id) }))
            .collect(),
    );
    Ok(value)
}

/// The N-P4 recipient reference of a partner principal.
pub fn recipient(environment_id: Uuid, principal_id: Uuid) -> String {
    format!("{PLATFORM_PRINCIPAL}:{environment_id}:{principal_id}")
}

/// Counts of one relay pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RelayReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    pub skipped: usize,
    pub expired: u64,
    pub not_configured: usize,
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 8)).min(120))
}

async fn settle(state: &AppState, id: Uuid, to: &str, outcome: Option<&str>, now: DateTime<Utc>) {
    if let Err(e) = db::exec(
        &state.db,
        "UPDATE messaging_partner_push_publications SET state = $2, outcome = $3, attempts = attempts + 1, \
           completed_at = $4 WHERE id = $1 AND state = 'PENDING'",
        vec![id.into(), to.into(), outcome.map(str::to_owned).into(), now.into()],
    )
    .await
    {
        tracing::warn!(error = %crate::db::summarize_db_error(&e), "partner wake-up not settled");
    }
}

async fn retry_later(state: &AppState, row: &Row, error: &str, now: DateTime<Utc>) {
    let _ = db::exec(
        &state.db,
        "UPDATE messaging_partner_push_publications SET attempts = attempts + 1, last_error = $2, next_attempt_at = $3 \
         WHERE id = $1 AND state = 'PENDING'",
        vec![
            row.id.into(),
            error.to_owned().into(),
            (now + backoff(row.attempts)).into(),
        ],
    )
    .await;
}

/// One relay pass: expires stale rows, then posts due ones (leased for a minute so
/// replicas never post the same row concurrently).
pub async fn relay_due(state: &AppState) -> Result<RelayReport> {
    let mut report = RelayReport::default();
    let now = state.now();
    report.expired = db::exec(
        &state.db,
        "UPDATE messaging_partner_push_publications SET state = 'EXPIRED', completed_at = $1 \
          WHERE state = 'PENDING' AND expires_at <= $1",
        vec![now.into()],
    )
    .await?;
    let rows: Vec<Row> = db::all(
        &state.db,
        "UPDATE messaging_partner_push_publications SET next_attempt_at = $2 \
          WHERE id IN (SELECT id FROM messaging_partner_push_publications \
                        WHERE state = 'PENDING' AND next_attempt_at <= $1 ORDER BY next_attempt_at \
                        LIMIT $3 FOR UPDATE SKIP LOCKED) \
         RETURNING id, outbox_event_id, tenant_id, application_id, environment_id, conversation_id, event_sequence, \
                   correlation_id, envelope, attempts, occurred_at, expires_at",
        vec![
            now.into(),
            (now + Duration::minutes(1)).into(),
            (BATCH as i64).into(),
        ],
    )
    .await?;
    let mut ready: Vec<(Row, Value)> = Vec::new();
    for row in rows {
        let envelope = match row.envelope.clone() {
            Some(e) => e,
            None => match build(state, &row).await {
                Ok(e) => {
                    db::exec(
                        &state.db,
                        "UPDATE messaging_partner_push_publications SET envelope = $2 WHERE id = $1",
                        vec![row.id.into(), e.clone().into()],
                    )
                    .await?;
                    e
                }
                Err("UNAVAILABLE") => {
                    retry_later(state, &row, "UNAVAILABLE", now).await;
                    report.retried += 1;
                    continue;
                }
                Err("INVALID") => {
                    settle(state, row.id, "DEAD", Some("INVALID_ENVELOPE"), now).await;
                    crate::metrics::inc(METRIC, &[("outcome", "dead")]);
                    report.dead += 1;
                    continue;
                }
                Err(reason) => {
                    settle(state, row.id, "SKIPPED", Some(reason), now).await;
                    crate::metrics::inc(METRIC, &[("outcome", "skipped")]);
                    report.skipped += 1;
                    continue;
                }
            },
        };
        ready.push((row, envelope));
    }
    if ready.is_empty() {
        return Ok(report);
    }
    let events: Vec<Value> = ready.iter().map(|(_, e)| e.clone()).collect();
    match state.partner.wakeups.ingest(&events, false).await {
        Ok(results) => {
            for (row, _) in &ready {
                let id = event_id(row);
                match results.iter().find(|r| r.event_id == id) {
                    Some(r) if matches!(r.outcome.as_str(), "ACCEPTED" | "DUPLICATE" | "HELD") => {
                        settle(state, row.id, "ACKED", Some(r.outcome.as_str()), now).await;
                        crate::metrics::inc(METRIC, &[("outcome", "acked")]);
                        report.acked += 1;
                    }
                    Some(_) => {
                        settle(state, row.id, "DEAD", Some("REJECTED"), now).await;
                        crate::metrics::inc(METRIC, &[("outcome", "dead")]);
                        report.dead += 1;
                    }
                    None => {
                        retry_later(state, row, "NO_RESULT", now).await;
                        report.retried += 1;
                    }
                }
            }
        }
        Err(PublishFailure::NotConfigured) => {
            // Rows wait (until they expire) for a configured Notification Service.
            report.not_configured += ready.len();
        }
        Err(PublishFailure::Rejected) => {
            for (row, _) in &ready {
                settle(state, row.id, "DEAD", Some("CONTRACT_REJECTED"), now).await;
                report.dead += 1;
            }
            crate::metrics::inc(METRIC, &[("outcome", "dead")]);
        }
        Err(PublishFailure::Unavailable) => {
            for (row, _) in &ready {
                retry_later(state, row, "UNAVAILABLE", now).await;
                report.retried += 1;
            }
            crate::metrics::inc(METRIC, &[("outcome", "retried")]);
        }
    }
    Ok(report)
}

/// The relay loop (spawned only with `MESSAGING_PARTNER_PUSH_ENABLED`).
pub async fn run(state: AppState, interval: StdDuration) {
    loop {
        match relay_due(&state).await {
            Ok(r) if r.acked + r.dead + r.skipped > 0 => tracing::info!(
                acked = r.acked,
                dead = r.dead,
                skipped = r.skipped,
                retried = r.retried,
                "partner wake-ups relayed"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "partner wake-up relay pass failed"),
        }
        tokio::time::sleep(interval).await;
    }
}
