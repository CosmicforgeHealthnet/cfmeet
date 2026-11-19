//! Meeting's publications to the Notification Service (roadmap N7 / N11; Document 4 §5.1,
//! §9.3, §9.5) for the STANDARD categories `MEETING_INVITATION`, `MEETING_NOTIFICATION`
//! and `ORGANIZATION_INVITATION`: the producer outbox `meeting_notification_publications`
//! (fence history, second migration) and its relay to I1
//! (`POST /internal/v1/notification-events`, `ingestNotificationEvents`).
//!
//! * **Routing** ([`route`]) runs inside the transaction that would write the legacy row
//!   (the invitation email in `email_deliveries`, or the conference job in
//!   `meeting_notifications`), once the caller has read a mode other than `LEGACY_ONLY`
//!   ([`fence::current_mode`]). It records the sticky decision
//!   ([`fence::route_message_capable`]) and tells the caller whether to write the legacy
//!   row (`OLD`). In `SHADOW` a newly made `OLD` decision also enqueues one shadow
//!   publication (posted with `shadow: true`: the service records it and never delivers
//!   it); a `NEW` decision (`FENCED_NEW`, inside the canary) enqueues one publication and
//!   no legacy row. A notification whose event cannot be published stays `OLD`: an
//!   audience larger than one envelope (`RECIPIENT_LIMIT`), or no complete catalogue
//!   envelope (`UNPUBLISHABLE`: an email-only recipient the service cannot address from an
//!   envelope, an invitation without a conference, no sealing key configured).
//! * **Envelope** ([`EventDraft`]): built with `packages/notification-client` at enqueue
//!   (the invitation token is never stored by Meeting, so it can only be sealed then, to
//!   the service's HPKE key `CF_NOTIFICATIONS_SEALING_KEY`), validated, stored, and re-posted
//!   byte-identically (the service de-duplicates on (producer, eventId)). Ids only: data
//!   keys are exactly the catalogue's; recipients are CF Meet account references
//!   (`cfmeet-user:`) or, for an organization invitation, the invitation's email principal
//!   with the address sealed. Never titles, names, addresses in clear, tokens or links.
//! * **Relay** ([`relay_due`], [`run`]): posts only when `CF_NOTIFICATIONS_BASE_URL` is set
//!   (otherwise rows wait PENDING until they expire), signed as `cfmeet.meeting-service`
//!   for audience `cfmeet.notifications` (`notifications-event.publish`), in batches of
//!   ≤ 100 rows (shadow copies and publications apart). `ACCEPTED`, `DUPLICATE` and `HELD`
//!   acknowledge the row (`ACKED`; a `NEW` decision records `ns_acked_at`, the
//!   responsibility boundary of §9.5); a per-event `REJECTED` or a contract refusal of the
//!   batch (4xx other than 401 / 403 / 408 / 429) dead-letters it (`DEAD`); 5xx, 401 / 403
//!   / 408 / 429 and transport failures retry with backoff until the row expires
//!   (`EXPIRED`, never posted). Settled rows lose their sealed part and are purged after
//!   [`RETENTION`] ([`maintain`]).
//!
//! | Variable | Meaning |
//! |---|---|
//! | `CF_NOTIFICATIONS_BASE_URL` | Notification Service base URL (unset: nothing is posted) |
//! | `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | the Meeting signing identity (required with the URL) |
//! | `CF_NOTIFICATIONS_SEALING_KEY` | the service's public HPKE key `<kid>:<base64url>` (unset: invitation emails are `UNPUBLISHABLE` and stay on the legacy path) |

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::{
    envelope::MAX_RECIPIENTS, EnvelopeBuilder, EnvelopeError, Producer, RouteRef, SealedPlaintext,
    SealingKey,
};
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient, InternalTargets},
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr, FromQueryResult,
    Statement, TransactionTrait,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::services::{
    notification_fence::{self as fence, DecisionReason, MeetingCategory, Route, RoutedMessage},
    ops_metrics::{inc, OPS_METRICS as OM},
};

/// I1, nested under the Notification Service's internal base URL.
pub const INGEST_PATH: &str = "/internal/v1/notification-events";
/// Most events in one I1 batch (contract `maxItems`).
pub const MAX_BATCH: usize = 100;
/// How long a publication may wait for the relay (at most; a reminder or an invitation
/// stops earlier, at its start / expiry).
pub const LIFETIME: Duration = Duration::hours(24);
/// How long a routing decision is kept (then 30 more days before the purge may delete it).
pub const DECISION_LIFETIME: Duration = Duration::days(7);
/// How long a settled publication is kept after `completed_at`.
pub const RETENTION: Duration = Duration::days(30);

/// The catalogue event types Meeting publishes for its STANDARD categories.
pub mod event_type {
    /// A reminder (dedupe key `reminder:<conference>:<start epoch>`, §9.9 phase 4.3a).
    pub const SCHEDULED: &str = "meeting-service.conference.scheduled";
    pub const RESCHEDULED: &str = "meeting-service.conference.rescheduled";
    pub const CANCELLED: &str = "meeting-service.conference.cancelled";
    pub const INVITATION: &str = "meeting-service.invitation.email-requested";
    pub const ORGANIZATION_INVITATION: &str =
        "meeting-service.organization-invitation.email-requested";
}

// ----------------------------------------------------------------------------- envelope

/// Everything one event needs except its id and routing decision.
#[derive(Debug, Clone)]
pub struct EventDraft {
    pub category: MeetingCategory,
    pub event_type: &'static str,
    pub subject_id: Uuid,
    pub subject_version: Option<i64>,
    /// Exactly the catalogue's data keys.
    pub data: Vec<(&'static str, Value)>,
    /// Recipient references (deduplicated by the caller).
    pub recipients: Vec<String>,
    pub dedupe_key: String,
    /// Today's ledger key format (`meeting-notification:…`, `meeting-invitation:…`,
    /// `org-invitation:…`).
    pub idempotency_key: String,
    pub cancel_key: Option<String>,
    /// `delivery.expiresAt`.
    pub delivery_expires_at: Option<DateTime<Utc>>,
    /// Sealed to the service key (invitation token / address); never stored in clear.
    pub secret: Option<SealedPlaintext>,
    /// The relay gives up after this instant (`EXPIRED`).
    pub publish_until: DateTime<Utc>,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// The envelope event id of a publication row.
pub fn event_id(row_id: Uuid) -> String {
    format!("evt_{}", row_id.simple())
}

/// Builds and validates the envelope of `draft` for `route`. `Err` names why the event
/// cannot be published (`RecipientLimit`: the audience does not fit one envelope;
/// `Unpublishable`: anything else).
pub fn build_envelope(
    draft: &EventDraft,
    row_id: Uuid,
    route: RouteRef,
    sealing: Option<&SealingKey>,
    occurred_at: DateTime<Utc>,
) -> Result<Value, DecisionReason> {
    if draft.recipients.is_empty() {
        return Err(DecisionReason::Unpublishable);
    }
    if draft.recipients.len() > MAX_RECIPIENTS {
        return Err(DecisionReason::RecipientLimit);
    }
    let mut builder = EnvelopeBuilder::new(
        Producer::MeetingService,
        draft.event_type,
        &rfc3339(occurred_at),
        route,
    )
    .map_err(|_| DecisionReason::Unpublishable)?
    .event_id(event_id(row_id))
    .correlation_id(format!("corr_{}", row_id.simple()))
    .subject(draft.subject_id, draft.subject_version)
    .dedupe_key(draft.dedupe_key.clone())
    .idempotency_key(draft.idempotency_key.clone());
    if let Some(k) = &draft.cancel_key {
        builder = builder.cancel_key(k.clone());
    }
    if let Some(at) = draft.delivery_expires_at {
        builder = builder.expires_at(rfc3339(at));
    }
    for (k, v) in &draft.data {
        builder = builder.data(k, v.clone());
    }
    for r in &draft.recipients {
        builder = builder.recipient(r.clone());
    }
    let built = match (&draft.secret, sealing) {
        (Some(secret), Some(key)) => builder.secret(secret.clone()).build_sealed(key),
        (Some(_), None) => return Err(DecisionReason::Unpublishable),
        (None, _) => builder.build(),
    };
    match built {
        Ok(envelope) => serde_json::to_value(envelope).map_err(|_| DecisionReason::Unpublishable),
        Err(EnvelopeError::TooLarge(_)) => Err(DecisionReason::RecipientLimit),
        Err(_) => Err(DecisionReason::Unpublishable),
    }
}

/// Whether `draft` can be published: built once against a worst-case decision (the real
/// one is never larger), so a `NEW` route is only taken by an event that fits.
fn capability(
    draft: &Result<EventDraft, DecisionReason>,
    sealing: Option<&SealingKey>,
    now: DateTime<Utc>,
) -> Result<(), DecisionReason> {
    let draft = draft.as_ref().map_err(|r| *r)?;
    let worst = RouteRef {
        decision_id: Uuid::max(),
        routing_version: i32::MAX,
        protocol: fence::ROUTING_PROTOCOL,
    };
    build_envelope(draft, Uuid::max(), worst, sealing, now).map(|_| ())
}

// ------------------------------------------------------------------------------ routing

/// What the routing of one notification asks the caller to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Routed {
    pub decision: RoutedMessage,
    /// `true`: write the legacy row now (a fresh `OLD` decision). `false`: a `NEW`
    /// decision (published instead), or a followed (sticky) decision whose message
    /// already exists.
    pub write_legacy: bool,
    /// A publication (shadow copy or `NEW`) was enqueued.
    pub published: bool,
}

/// Routes one notification keyed `key` inside the caller's transaction (§9.3) and
/// enqueues its publication when the decision asks for one. `draft` is `Err(reason)` when
/// the notification has no event (it then stays `OLD`). Call only outside `LEGACY_ONLY`.
pub async fn route<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    draft: Result<EventDraft, DecisionReason>,
    category: MeetingCategory,
    sealing: Option<&SealingKey>,
    now: DateTime<Utc>,
) -> Result<Routed, DbErr> {
    route_with_in_app(conn, key, draft, None, category, sealing, now).await
}

/// What one fresh decision publishes: `Some(shadow)` (one row), or nothing. `in_app`: an
/// in-app event of an OLD decision (audit M4) can be published.
pub fn publication_kind(
    route: Route,
    decision_shadow: bool,
    capable: bool,
    in_app: bool,
) -> Option<bool> {
    match route {
        Route::New => Some(false),
        // Audit M4: the OLD decision keeps its legacy email; its in-app event is delivered
        // (non-shadow) instead of a shadow copy. One publication per decision either way.
        Route::Old if in_app => Some(false),
        // SHADOW: the service records a copy of every OLD decision it can be told about.
        Route::Old if decision_shadow && capable => Some(true),
        Route::Old => None,
    }
}

/// [`route`] plus the audit M4 in-app path. `in_app` is the event of the same notice
/// addressed to its CF Meet account recipients only (`None`: the path is off). When the
/// decision is a fresh `OLD` (the legacy row is written and emailed exactly as today) and
/// that event can be built, it is published as a delivered event: the Notification Service
/// delivers it on the channels it has ACTIVE for the category (IN_APP; its EMAIL row is
/// never ACTIVE while the path is on, so no second email exists). A `NEW` decision
/// publishes the full `draft` as before; `in_app` never influences the decision itself.
pub async fn route_with_in_app<C: ConnectionTrait>(
    conn: &C,
    key: &str,
    draft: Result<EventDraft, DecisionReason>,
    in_app: Option<Result<EventDraft, DecisionReason>>,
    category: MeetingCategory,
    sealing: Option<&SealingKey>,
    now: DateTime<Utc>,
) -> Result<Routed, DbErr> {
    let capable = capability(&draft, sealing, now);
    let decision =
        fence::route_message_capable(conn, key, category, now + DECISION_LIFETIME, capable).await?;
    if decision.sticky {
        return Ok(Routed {
            decision,
            write_legacy: false,
            published: false,
        });
    }
    let in_app = in_app
        .filter(|d| capability(d, sealing, now).is_ok())
        .and_then(Result::ok)
        .filter(|_| decision.route == Route::Old);
    let kind = publication_kind(
        decision.route,
        decision.shadow,
        capable.is_ok(),
        in_app.is_some(),
    );
    if decision.route == Route::Old && decision.shadow && capable.is_err() && in_app.is_none() {
        inc(&OM.notification_publications_unpublishable);
    }
    if let Some(shadow) = kind {
        let event = match in_app {
            Some(event) => event,
            None => draft.map_err(|_| DbErr::Custom("publication without an event".into()))?,
        };
        enqueue(conn, &event, &decision, shadow, sealing, now).await?;
    }
    Ok(Routed {
        decision,
        write_legacy: decision.route == Route::Old,
        published: kind.is_some(),
    })
}

/// Writes one publication row (idempotent per decision).
async fn enqueue<C: ConnectionTrait>(
    conn: &C,
    draft: &EventDraft,
    decision: &RoutedMessage,
    shadow: bool,
    sealing: Option<&SealingKey>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let id = Uuid::new_v4();
    let envelope = build_envelope(
        draft,
        id,
        RouteRef {
            decision_id: decision.decision_id,
            routing_version: decision.routing_version,
            protocol: fence::ROUTING_PROTOCOL,
        },
        sealing,
        now,
    )
    .map_err(|_| DbErr::Custom("the publication envelope cannot be built".into()))?;
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO meeting_notification_publications (id, event_id, decision_id, routing_version, category,
             event_type, shadow, envelope, next_attempt_at, created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10)
         ON CONFLICT DO NOTHING",
        [
            id.into(),
            event_id(id).into(),
            decision.decision_id.into(),
            decision.routing_version.into(),
            draft.category.as_str().into(),
            draft.event_type.into(),
            shadow.into(),
            envelope.into(),
            now.into(),
            draft.publish_until.min(now + LIFETIME).max(now).into(),
        ],
    ))
    .await?;
    Ok(())
}

/// The service's public sealing key from `CF_NOTIFICATIONS_SEALING_KEY`
/// (`<kid>:<base64url X25519 public key>`). `Ok(None)`: not configured. `Err`: invalid
/// (startup fails closed).
pub fn sealing_key_from_env() -> Result<Option<SealingKey>, String> {
    match std::env::var("CF_NOTIFICATIONS_SEALING_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        None => Ok(None),
        Some(v) => SealingKey::parse(&v).map(Some).map_err(|_| {
            "CF_NOTIFICATIONS_SEALING_KEY must be <kid>:<base64url public key>".into()
        }),
    }
}

// --------------------------------------------------------------------------------- sink

/// One per-event answer of I1.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestResult {
    pub event_id: String,
    pub outcome: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFailure {
    /// Retry with backoff (5xx, 401, 403, 408, 429, timeout, unreachable, malformed answer).
    Unavailable,
    /// The service refused the batch for good (a contract error): dead letter.
    Rejected,
    /// No Notification Service target configured: rows wait.
    NotConfigured,
}

impl PublishFailure {
    fn code(self) -> &'static str {
        match self {
            PublishFailure::Unavailable => "UNAVAILABLE",
            PublishFailure::Rejected => "CONTRACT_REJECTED",
            PublishFailure::NotConfigured => "NOT_CONFIGURED",
        }
    }
}

/// Where publications go (I1).
#[async_trait]
pub trait NotificationEventSink: Send + Sync {
    /// Posts one batch (≤ [`MAX_BATCH`]); `shadow` marks a batch of SHADOW copies.
    async fn ingest(
        &self,
        events: &[Value],
        shadow: bool,
    ) -> Result<Vec<IngestResult>, PublishFailure>;
}

/// The I1 request body. `shadow` is sent only when set.
pub fn batch_body(events: &[Value], shadow: bool) -> Value {
    let mut body = json!({ "events": events });
    if shadow {
        body["shadow"] = json!(true);
    }
    body
}

/// The service-authenticated I1 of the Notification Service (`cfmeet.notifications`,
/// `notifications-event.publish`), signed as `cfmeet.meeting-service`.
pub struct NotificationServiceEvents {
    client: Arc<InternalServiceClient>,
}

#[derive(Deserialize)]
struct IngestAnswer {
    results: Vec<IngestResult>,
}

impl NotificationServiceEvents {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }

    /// `Ok(None)` without `CF_NOTIFICATIONS_BASE_URL` (nothing is posted). `Err`: configured
    /// but invalid (startup fails closed). Same signing identity as the user relay.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(url) = std::env::var("CF_NOTIFICATIONS_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
        else {
            return Ok(None);
        };
        let required = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("{k} is required with CF_NOTIFICATIONS_BASE_URL"))
        };
        let runtime = RuntimeEnvironment::parse(&required("CF_RUNTIME_ENVIRONMENT")?)
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT must be local, test, staging or production")?;
        let key = ServiceSigningKey::from_pkcs8_pem_file(
            KeyId::parse(&required("CF_SERVICE_AUTH_KEY_ID")?)
                .map_err(|_| "invalid CF_SERVICE_AUTH_KEY_ID")?,
            &PathBuf::from(required("CF_SERVICE_AUTH_PRIVATE_KEY_FILE")?),
        )
        .map_err(|_| "the Meeting signing key is unavailable")?;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            runtime,
            key,
            TokenTtl::seconds(60).map_err(|_| "invalid token TTL")?,
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(runtime);
        targets
            .insert(ServiceId::notifications(), url.trim())
            .map_err(|e| format!("CF_NOTIFICATIONS_BASE_URL: {e}"))?;
        let client = InternalServiceClient::new(signer, targets, StdDuration::from_secs(10))
            .map_err(|e| format!("Notification Service client: {e}"))?;
        Ok(Some(Self::new(Arc::new(client))))
    }
}

#[async_trait]
impl NotificationEventSink for NotificationServiceEvents {
    async fn ingest(
        &self,
        events: &[Value],
        shadow: bool,
    ) -> Result<Vec<IngestResult>, PublishFailure> {
        let request = OutboundServiceRequest::new_operation(ServiceId::notifications());
        match self
            .client
            .exchange_json(
                &request,
                Method::POST,
                INGEST_PATH,
                Some(&batch_body(events, shadow)),
            )
            .await
        {
            Ok(r) if r.status() == 200 => r
                .json::<IngestAnswer>()
                .map(|a| a.results)
                .map_err(|_| PublishFailure::Unavailable),
            Ok(r) if r.status() >= 500 || matches!(r.status(), 401 | 403 | 408 | 429) => {
                Err(PublishFailure::Unavailable)
            }
            Ok(_) => Err(PublishFailure::Rejected),
            Err(InternalCallError::UnknownTarget(_)) => Err(PublishFailure::NotConfigured),
            Err(_) => Err(PublishFailure::Unavailable),
        }
    }
}

// -------------------------------------------------------------------------------- relay

#[derive(Debug, Clone, FromQueryResult)]
struct Row {
    id: Uuid,
    event_id: String,
    decision_id: Uuid,
    shadow: bool,
    envelope: Value,
    attempts: i32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublicationReport {
    pub acked: usize,
    pub retried: usize,
    pub dead: usize,
    pub expired: u64,
    /// Rows left waiting because no Notification Service target is configured.
    pub not_configured: usize,
}

fn backoff(attempts: i32) -> Duration {
    Duration::seconds((1i64 << attempts.clamp(0, 9)).min(300))
}

/// A machine reason from the service, kept only when it has the recorded shape.
fn reason_code(reason: Option<&str>, fallback: &'static str) -> String {
    reason
        .filter(|r| {
            (2..=64).contains(&r.len())
                && r.as_bytes()[0].is_ascii_uppercase()
                && r.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or(fallback)
        .to_owned()
}

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

async fn settle(
    txn: &DatabaseTransaction,
    row: &Row,
    state: &str,
    outcome: Option<&str>,
    error: Option<String>,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    txn.execute(stmt(
        "UPDATE meeting_notification_publications
            SET state = $2, outcome = $3, last_error = $4, completed_at = $5,
                attempts = attempts + 1, envelope = envelope - 'sealed'
          WHERE id = $1",
        vec![
            row.id.into(),
            state.into(),
            outcome.map(str::to_owned).into(),
            error.into(),
            now.into(),
        ],
    ))
    .await?;
    if state == "ACKED" && !row.shadow {
        // §9.5 / F5: the service committed a NEW decision's event.
        txn.execute(stmt(
            "UPDATE notification_route_decisions SET ns_acked_at = $2
              WHERE decision_id = $1 AND route = 'NEW' AND ns_acked_at IS NULL",
            vec![row.decision_id.into(), now.into()],
        ))
        .await?;
    }
    match state {
        "ACKED" => inc(&OM.notification_publications_acked),
        _ => inc(&OM.notification_publications_dead),
    }
    Ok(())
}

async fn retry(
    txn: &DatabaseTransaction,
    row: &Row,
    error: &str,
    counted: bool,
    now: DateTime<Utc>,
) -> Result<(), DbErr> {
    let attempts = row.attempts + i32::from(counted);
    let next = if counted {
        now + backoff(attempts)
    } else {
        now + Duration::seconds(60)
    };
    txn.execute(stmt(
        "UPDATE meeting_notification_publications SET attempts = $2, last_error = $3, next_attempt_at = $4
          WHERE id = $1",
        vec![row.id.into(), attempts.into(), error.into(), next.into()],
    ))
    .await?;
    if counted {
        inc(&OM.notification_publications_retried);
    }
    Ok(())
}

/// One relay pass over at most `limit` due rows, in batches of ≤ [`MAX_BATCH`].
/// Multi-instance safe: a batch's rows are locked (`FOR UPDATE SKIP LOCKED`) for its
/// attempt and settled in the same transaction; a crash mid-attempt leaves them PENDING
/// and the re-post is answered `DUPLICATE` (acknowledged). Expired PENDING rows end
/// `EXPIRED` first and are never posted.
pub async fn relay_due(
    db: &DatabaseConnection,
    sink: &dyn NotificationEventSink,
    limit: usize,
    now: DateTime<Utc>,
) -> Result<PublicationReport, DbErr> {
    let mut report = PublicationReport {
        expired: expire_overdue(db, now).await?,
        ..Default::default()
    };
    let mut remaining = limit;
    while remaining > 0 {
        let batch = remaining.min(MAX_BATCH);
        let txn = db.begin().await?;
        let rows = Row::find_by_statement(stmt(
            "SELECT id, event_id, decision_id, shadow, envelope, attempts
               FROM meeting_notification_publications
              WHERE state = 'PENDING' AND next_attempt_at <= $1 AND expires_at > $1
              ORDER BY next_attempt_at, created_at LIMIT $2 FOR UPDATE SKIP LOCKED",
            vec![now.into(), (batch as i64).into()],
        ))
        .all(&txn)
        .await?;
        if rows.is_empty() {
            txn.rollback().await?;
            break;
        }
        remaining = remaining.saturating_sub(rows.len());
        let full = rows.len() == batch;
        let mut stop = false;
        // `shadow` is a batch flag: shadow copies and publications are posted apart.
        for shadow in [true, false] {
            let group: Vec<&Row> = rows.iter().filter(|r| r.shadow == shadow).collect();
            if group.is_empty() {
                continue;
            }
            let events: Vec<Value> = group.iter().map(|r| r.envelope.clone()).collect();
            match sink.ingest(&events, shadow).await {
                Ok(results) => {
                    let by_id: HashMap<&str, &IngestResult> =
                        results.iter().map(|r| (r.event_id.as_str(), r)).collect();
                    for row in group {
                        match by_id.get(row.event_id.as_str()) {
                            Some(r)
                                if matches!(
                                    r.outcome.as_str(),
                                    "ACCEPTED" | "DUPLICATE" | "HELD"
                                ) =>
                            {
                                settle(&txn, row, "ACKED", Some(&r.outcome), None, now).await?;
                                report.acked += 1;
                            }
                            Some(r) if r.outcome == "REJECTED" => {
                                let reason = reason_code(r.reason.as_deref(), "REJECTED");
                                settle(&txn, row, "DEAD", Some("REJECTED"), Some(reason), now)
                                    .await?;
                                report.dead += 1;
                            }
                            // No (or an unexpected) answer for this event: try again.
                            _ => {
                                retry(&txn, row, "NO_RESULT", true, now).await?;
                                report.retried += 1;
                            }
                        }
                    }
                }
                Err(f @ PublishFailure::Rejected) => {
                    for row in group {
                        settle(&txn, row, "DEAD", None, Some(f.code().into()), now).await?;
                        report.dead += 1;
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::Unavailable) => {
                    for row in group {
                        retry(&txn, row, f.code(), true, now).await?;
                        report.retried += 1;
                    }
                    stop = true;
                }
                Err(f @ PublishFailure::NotConfigured) => {
                    for row in group {
                        retry(&txn, row, f.code(), false, now).await?;
                        report.not_configured += 1;
                    }
                    stop = true;
                }
            }
        }
        txn.commit().await?;
        if stop || !full {
            break;
        }
    }
    Ok(report)
}

/// Ends every PENDING row past its expiry as `EXPIRED` (never posted). Returns how many.
async fn expire_overdue(db: &DatabaseConnection, now: DateTime<Utc>) -> Result<u64, DbErr> {
    let expired = db
        .execute(stmt(
            "UPDATE meeting_notification_publications
                SET state = 'EXPIRED', last_error = 'EXPIRED', completed_at = $1,
                    envelope = envelope - 'sealed'
              WHERE state = 'PENDING' AND expires_at <= $1",
            vec![now.into()],
        ))
        .await?
        .rows_affected();
    for _ in 0..expired {
        inc(&OM.notification_publications_expired);
    }
    Ok(expired)
}

fn undefined_table(e: &DbErr) -> bool {
    let runtime = match e {
        DbErr::Query(e) | DbErr::Exec(e) => e,
        _ => return false,
    };
    matches!(runtime, sea_orm::RuntimeErr::SqlxError(s)
        if s.as_database_error().and_then(|d| d.code()).as_deref() == Some("42P01"))
}

/// Maintenance, whether or not the relay runs: expires overdue PENDING rows, deletes at
/// most `batch` settled rows older than [`RETENTION`], and at most `batch` expired
/// decisions of the three STANDARD categories past their 30-day grace (never one an
/// outbox row still references). A database without the fence history is a no-op.
/// Returns (expired, purged publications, purged decisions).
pub async fn maintain(
    db: &DatabaseConnection,
    batch: i64,
    now: DateTime<Utc>,
) -> Result<(u64, u64, u64), DbErr> {
    let expired = match expire_overdue(db, now).await {
        Err(e) if undefined_table(&e) => return Ok((0, 0, 0)),
        other => other?,
    };
    let purged = db
        .execute(stmt(
            "DELETE FROM meeting_notification_publications WHERE id IN (
                 SELECT id FROM meeting_notification_publications
                  WHERE state <> 'PENDING' AND completed_at < $1 ORDER BY completed_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected();
    let decisions = db
        .execute(stmt(
            "DELETE FROM notification_route_decisions WHERE idempotency_key IN (
                 SELECT d.idempotency_key FROM notification_route_decisions d
                  WHERE d.category IN ('MEETING_INVITATION', 'MEETING_NOTIFICATION', 'ORGANIZATION_INVITATION')
                    AND d.expires_at < $1
                    AND NOT EXISTS (SELECT 1 FROM account_notification_outbox o WHERE o.decision_id = d.decision_id)
                    AND NOT EXISTS (SELECT 1 FROM meeting_notification_publications p
                                     WHERE p.decision_id = d.decision_id AND p.state = 'PENDING')
                  ORDER BY d.expires_at LIMIT $2)",
            vec![(now - RETENTION).into(), batch.into()],
        ))
        .await?
        .rows_affected();
    Ok((expired, purged, decisions))
}

/// The background publication worker: relays due rows when a sink is configured
/// (`CF_NOTIFICATIONS_BASE_URL`), and runs [`maintain`] hourly in every case.
pub async fn run(
    db: DatabaseConnection,
    sink: Option<Arc<dyn NotificationEventSink>>,
    interval: StdDuration,
) {
    let mut last_maintenance: Option<DateTime<Utc>> = None;
    loop {
        let now = Utc::now();
        if let Some(sink) = &sink {
            match relay_due(&db, sink.as_ref(), 500, now).await {
                Ok(_) => {}
                Err(e) if undefined_table(&e) => {}
                Err(e) => tracing::warn!("notification publication relay pass failed: {e}"),
            }
        }
        if last_maintenance.is_none_or(|t| now - t >= Duration::hours(1)) {
            last_maintenance = Some(now);
            if let Err(e) = maintain(&db, 500, now).await {
                tracing::warn!("notification publication maintenance failed: {e}");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod in_app_tests {
    use super::*;

    #[test]
    fn one_publication_per_decision() {
        // Without the in-app path: today's rules.
        assert_eq!(
            publication_kind(Route::New, false, true, false),
            Some(false)
        );
        assert_eq!(publication_kind(Route::Old, true, true, false), Some(true));
        assert_eq!(publication_kind(Route::Old, true, false, false), None);
        assert_eq!(publication_kind(Route::Old, false, true, false), None);
        // In-app path: an OLD decision's event is delivered instead of shadow-copied.
        assert_eq!(publication_kind(Route::Old, true, true, true), Some(false));
        assert_eq!(
            publication_kind(Route::Old, false, false, true),
            Some(false)
        );
        assert_eq!(publication_kind(Route::New, false, true, true), Some(false));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(recipients: Vec<String>) -> EventDraft {
        let conference = Uuid::new_v4();
        let start = Utc::now() + Duration::hours(2);
        EventDraft {
            category: MeetingCategory::MeetingNotification,
            event_type: event_type::RESCHEDULED,
            subject_id: conference,
            subject_version: Some(i64::from(i32::MAX)),
            data: vec![
                ("conferenceId", json!(conference.to_string())),
                ("startsAt", json!(rfc3339(start))),
            ],
            recipients,
            dedupe_key: format!("rescheduled:{conference}:{}", i32::MAX),
            idempotency_key: format!("meeting-notification:rescheduled:{conference}:{}", i32::MAX),
            cancel_key: None,
            delivery_expires_at: None,
            secret: None,
            publish_until: start,
        }
    }

    fn accounts(n: usize) -> Vec<String> {
        (0..n)
            .map(|_| format!("cfmeet-user:{}", Uuid::new_v4()))
            .collect()
    }

    #[test]
    fn capability_follows_the_envelope_limits() {
        let now = Utc::now();
        assert_eq!(capability(&Ok(draft(accounts(1))), None, now), Ok(()));
        assert_eq!(capability(&Ok(draft(accounts(40))), None, now), Ok(()));
        assert_eq!(
            capability(&Ok(draft(Vec::new())), None, now),
            Err(DecisionReason::Unpublishable)
        );
        // The 4 KiB size limit binds well before the 200-recipient count.
        assert_eq!(
            capability(&Ok(draft(accounts(150))), None, now),
            Err(DecisionReason::RecipientLimit)
        );
        assert_eq!(
            capability(&Ok(draft(accounts(MAX_RECIPIENTS + 1))), None, now),
            Err(DecisionReason::RecipientLimit)
        );
        assert_eq!(
            capability(&Err(DecisionReason::Unpublishable), None, now),
            Err(DecisionReason::Unpublishable)
        );
        // A secret without a sealing key cannot be published.
        let mut sealed = draft(accounts(1));
        sealed.secret = Some(SealedPlaintext::default().with_token("inv_0123456789abcdefghij"));
        assert_eq!(
            capability(&Ok(sealed), None, now),
            Err(DecisionReason::Unpublishable)
        );
    }

    #[test]
    fn the_batch_flag_is_sent_only_for_shadow_copies() {
        let e = vec![json!({"eventId": "evt_x"})];
        assert_eq!(batch_body(&e, false), json!({ "events": e }));
        assert_eq!(batch_body(&e, true), json!({ "events": e, "shadow": true }));
    }

    #[test]
    fn service_reasons_are_recorded_only_in_their_machine_shape() {
        assert_eq!(
            reason_code(Some("ROUTE_NOT_NEW"), "REJECTED"),
            "ROUTE_NOT_NEW"
        );
        for bad in ["", "x", "route not new", "A\u{7}B", &"A".repeat(65)] {
            assert_eq!(reason_code(Some(bad), "REJECTED"), "REJECTED", "{bad:?}");
        }
    }

    #[test]
    fn backoff_grows_and_is_bounded() {
        assert_eq!(backoff(1), Duration::seconds(2));
        assert!(backoff(5) > backoff(4));
        assert_eq!(backoff(40), Duration::seconds(300));
    }
}
