//! Environment-scoped meeting events to the Developer Platform (Part VI §53).
//!
//! **Staging (transactional).** When a meeting-domain fact is written to the outbox
//! ([`crate::services::meeting_events::emit_to`]) and its type is publishable, ONE more
//! outbox row is written in the SAME transaction on the `PUBLIC_EVENT` channel — but only
//! when the meeting space is owned by a Platform Access application environment (a single
//! `INSERT … SELECT` from `meeting_spaces`; user-owned meetings never produce one). The
//! row holds the platform envelope's type, owning scope (tenant, application,
//! environment — from the space, never from a request), the request's correlation id (the
//! gateway's signed one for forwarded requests) and a payload of safe identifiers and
//! lifecycle codes only. Realtime clients never see this channel.
//!
//! | internal fact | envelope type (`producer_service` = `cfmeet.meeting-service`) |
//! |---|---|
//! | `meeting_space.created` | `meeting-service.meeting.created` |
//! | `conference.started` | `meeting-service.meeting.started` |
//! | `conference.ended` / `conference.failed` | `meeting-service.meeting.ended` |
//! | `participant.joined` / `participant.left` | `meeting-service.participant.joined` / `.left` |
//! | `recording.ready` / `recording.deleted` | `meeting-service.recording.ready` / `.deleted` |
//! | `artifact.ready` / `artifact.deleted` | `meeting-service.artifact.ready` / `.deleted` |
//!
//! Payload keys (the Developer Platform's meeting allow-list): `meeting_id` (the meeting
//! space), `conference_id`, `participant_id`, `recording_id`, `artifact_id`,
//! `artifact_kind`, `role`, `status`, `ended_reason`, `duration_ms`. Every value is an
//! opaque UUID, a bounded upper-case code or a non-negative integer; anything else is
//! dropped. Never: display names, emails, join links / codes, passcodes, invitation
//! tokens, media credentials, provider (LiveKit) ids, storage keys or URLs.
//!
//! **Publication.** [`PublicEventPublisher`] claims `PUBLIC_EVENT` rows (crash-safe
//! leases, [`crate::services::outbox`]) and POSTs each envelope (stable `event_id` =
//! `evt_<outbox id>`, so retries and replays de-duplicate at the Developer Platform) to
//! `POST /internal/v1/platform/events` (`developer-event.publish`), continuing the
//! committed correlation id. A 4xx refusal other than `RATE_LIMITED` dead-letters the row;
//! anything else is retried with capped backoff.
//!
//! **`MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION`** ([`PublicEventsDestination`]):
//!
//! * `none` (default) — nothing is staged and nothing leaves the Meeting Service: exactly
//!   today's behaviour. It is the default although the Developer Platform ingests in
//!   shadow mode until its delivery cut-over, because publishing needs the Developer
//!   Platform URL and the service signing key that existing deployments do not configure.
//! * `developer-platform` — stage and publish (requires `CF_DEVELOPER_PLATFORM_BASE_URL`).

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, OutboundServiceRequest, RuntimeEnvironment, ServiceId,
};
use chrono::SecondsFormat;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    domain::OutboxChannel,
    error::ApiResult,
    middleware::request_id::current_request_id,
    models::outbox_events,
    services::{meeting_events::OutboxRecord, outbox},
};

/// The platform-event ingress path of the Developer Platform.
pub const PLATFORM_EVENTS_PATH: &str = "/internal/v1/platform/events";
/// Producer identity of every envelope.
pub const PRODUCER: &str = "cfmeet.meeting-service";

/// `MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublicEventsDestination {
    /// Nothing is staged or published (default; unchanged behaviour).
    #[default]
    None,
    /// The Developer Platform (central customer webhooks).
    DeveloperPlatform,
}

impl PublicEventsDestination {
    pub const VARIABLE: &'static str = "MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION";

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "none" => Some(Self::None),
            "developer-platform" => Some(Self::DeveloperPlatform),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::DeveloperPlatform => "developer-platform",
        }
    }
}

static STAGING: AtomicBool = AtomicBool::new(false);

/// Turns staging on for this process (the `developer-platform` destination). Set once at
/// startup; off by default.
pub fn set_staging(enabled: bool) {
    STAGING.store(enabled, Ordering::Release);
}

pub fn staging_enabled() -> bool {
    STAGING.load(Ordering::Acquire)
}

/// The envelope type of a publishable internal fact.
pub fn public_type(internal: &str) -> Option<&'static str> {
    Some(match internal {
        "meeting_space.created" => "meeting-service.meeting.created",
        "conference.started" => "meeting-service.meeting.started",
        "conference.ended" | "conference.failed" => "meeting-service.meeting.ended",
        "participant.joined" => "meeting-service.participant.joined",
        "participant.left" => "meeting-service.participant.left",
        "recording.ready" => "meeting-service.recording.ready",
        "recording.deleted" => "meeting-service.recording.deleted",
        "artifact.ready" => "meeting-service.artifact.ready",
        "artifact.deleted" => "meeting-service.artifact.deleted",
        _ => return None,
    })
}

fn is_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_uppercase()
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// An opaque UUID (lower-case), a bounded upper-case code or (numeric keys) a
/// non-negative integer — nothing else may leave.
fn safe(v: &Value, numeric: bool) -> Option<Value> {
    match v {
        Value::String(s) if s.len() == 36 && Uuid::parse_str(s).is_ok() => {
            Some(Value::String(s.to_ascii_lowercase()))
        }
        Value::String(s) if !numeric && is_code(s) => Some(v.clone()),
        Value::Number(n) if numeric && n.as_u64().is_some() => Some(v.clone()),
        _ => None,
    }
}

/// The safe payload of a publishable fact (`None`: not publishable).
pub fn public_payload(record: &OutboxRecord<'_>) -> Option<(&'static str, Value)> {
    let event_type = public_type(record.event_type)?;
    let space = record.meeting_space_id?;
    let p = &record.payload;
    let mut out = Map::new();
    out.insert("meeting_id".into(), json!(space));
    if let Some(c) = record.conference_id {
        out.insert("conference_id".into(), json!(c));
    }
    let mut copy = |public: &str, internal: &str, numeric: bool| {
        if let Some(v) = p.get(internal).and_then(|v| safe(v, numeric)) {
            out.insert(public.to_string(), v);
        }
    };
    match record.event_type {
        "conference.started" | "conference.ended" | "conference.failed" => {
            copy("status", "status", false);
        }
        "participant.joined" | "participant.left" => {
            copy("participant_id", "participantId", false);
            copy("role", "role", false);
        }
        "recording.ready" | "recording.deleted" => {
            copy("recording_id", "recordingId", false);
            copy("status", "status", false);
        }
        "artifact.ready" | "artifact.deleted" => {
            copy("artifact_id", "artifactId", false);
            copy("artifact_kind", "artifactType", false);
            copy("status", "status", false);
            copy("duration_ms", "durationMs", true);
        }
        _ => {}
    }
    if record.event_type == "conference.failed" {
        out.insert("ended_reason".into(), json!("FAILED"));
    }
    if record.event_type.starts_with("recording.") && !out.contains_key("recording_id") {
        out.insert("recording_id".into(), json!(record.aggregate_id));
    }
    if record.event_type.starts_with("artifact.") && !out.contains_key("artifact_id") {
        out.insert("artifact_id".into(), json!(record.aggregate_id));
    }
    Some((event_type, Value::Object(out)))
}

/// Stages the platform envelope of `record` on the caller's transaction when its meeting
/// space is environment-owned (a no-op otherwise, and for every non-publishable fact).
pub async fn stage<C: ConnectionTrait>(conn: &C, record: &OutboxRecord<'_>) -> ApiResult<()> {
    let Some((event_type, payload)) = public_payload(record) else {
        return Ok(());
    };
    let Some(space) = record.meeting_space_id else {
        return Ok(());
    };
    let correlation = super::current_correlation()
        .unwrap_or_else(CorrelationId::generate)
        .as_str()
        .to_string();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"INSERT INTO outbox_events
                (id, aggregate_type, aggregate_id, event_type, payload, meeting_space_id,
                 conference_id, status, attempts, available_at, request_id, created_at, channel)
           SELECT $1, $2, $3, $4,
                  jsonb_build_object(
                      'event_type', $5::text,
                      'correlation_id', $6::text,
                      'payload', $7::jsonb,
                      'ownership', jsonb_build_object(
                          'tenant_id', s.platform_tenant_id,
                          'application_id', s.platform_application_id,
                          'application_environment_id', s.platform_environment_id)),
                  s.id, $8, 'PENDING', 0, now(), $9, now(), 'PUBLIC_EVENT'
             FROM meeting_spaces s
            WHERE s.id = $10 AND s.platform_environment_id IS NOT NULL
              AND s.owner_user_id IS NULL"#,
        [
            Uuid::new_v4().into(),
            record.aggregate_type.into(),
            record.aggregate_id.into(),
            event_type.into(),
            event_type.into(),
            correlation.into(),
            payload.into(),
            record.conference_id.into(),
            current_request_id().into(),
            space.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// The platform envelope (`cfmeet.platform.event-envelope` v1) of a staged row.
pub fn envelope(row: &outbox_events::Model, runtime: RuntimeEnvironment) -> Option<Value> {
    let staged = &row.payload;
    let event_type = staged.get("event_type")?.as_str()?;
    let correlation = staged.get("correlation_id")?.as_str()?;
    CorrelationId::parse(correlation).ok()?;
    let ownership = staged.get("ownership")?;
    for k in ["tenant_id", "application_id", "application_environment_id"] {
        Uuid::parse_str(ownership.get(k)?.as_str()?).ok()?;
    }
    let payload = staged.get("payload").filter(|p| p.is_object())?;
    Some(json!({
        "event_id": format!("evt_{}", row.id.simple()),
        "event_type": event_type,
        "schema_version": 1,
        "occurred_at": row.created_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        "producer_service": PRODUCER,
        "runtime_environment": runtime.as_str(),
        "correlation_id": correlation,
        "ownership": ownership,
        "payload": payload,
    }))
}

/// Publishes staged `PUBLIC_EVENT` rows to the Developer Platform.
pub struct PublicEventPublisher {
    db: DatabaseConnection,
    client: Arc<InternalServiceClient>,
    runtime: RuntimeEnvironment,
    holder: String,
}

/// Outcome counts of one publication pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PublishReport {
    pub published: usize,
    pub retried: usize,
    pub dead_lettered: usize,
}

impl PublicEventPublisher {
    pub fn new(
        db: DatabaseConnection,
        client: Arc<InternalServiceClient>,
        runtime: RuntimeEnvironment,
    ) -> Self {
        Self {
            db,
            client,
            runtime,
            holder: outbox::holder_id("public-events"),
        }
    }

    /// One pass: claim due rows and publish each.
    pub async fn run_once(&self) -> ApiResult<PublishReport> {
        let rows = outbox::claim(
            &self.db,
            OutboxChannel::PublicEvent,
            None,
            50,
            30,
            &self.holder,
        )
        .await?;
        let mut report = PublishReport::default();
        for row in rows {
            let Some(envelope) = envelope(&row, self.runtime) else {
                outbox::mark_failed(&self.db, row.id, &self.holder, "MALFORMED_ENVELOPE").await?;
                report.dead_lettered += 1;
                continue;
            };
            let correlation = envelope["correlation_id"]
                .as_str()
                .and_then(|c| CorrelationId::parse(c).ok())
                .unwrap_or_else(CorrelationId::generate);
            let request =
                OutboundServiceRequest::continuing(ServiceId::developer_platform(), &correlation);
            match self
                .client
                .post_json(&request, PLATFORM_EVENTS_PATH, &envelope)
                .await
            {
                Ok(_) => {
                    outbox::mark_published(&self.db, row.id, &self.holder).await?;
                    report.published += 1;
                }
                Err(InternalCallError::Status {
                    status: 400..=499,
                    code,
                    ..
                }) if code.as_deref() != Some("RATE_LIMITED") => {
                    tracing::warn!(outbox_id = %row.id, code = code.as_deref().unwrap_or(""),
                        "public meeting event refused by the Developer Platform; dead-lettered");
                    outbox::mark_failed(
                        &self.db,
                        row.id,
                        &self.holder,
                        &format!("REFUSED {}", code.unwrap_or_default()),
                    )
                    .await?;
                    report.dead_lettered += 1;
                }
                Err(other) => {
                    tracing::info!(outbox_id = %row.id, error = %other,
                        "public meeting event not delivered yet; retrying");
                    outbox::mark_retry(&self.db, &row, &self.holder, &other.to_string(), true)
                        .await?;
                    report.retried += 1;
                }
            }
        }
        Ok(report)
    }
}

/// Runs the publisher every `interval` until the process exits.
pub fn spawn_public_event_publisher(
    publisher: PublicEventPublisher,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = publisher.run_once().await {
                tracing::warn!("public meeting event publisher pass failed: {e}");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record<'a>(event_type: &'a str, payload: Value) -> OutboxRecord<'a> {
        OutboxRecord {
            aggregate_type: "x",
            aggregate_id: Uuid::nil(),
            event_type,
            meeting_space_id: Some(Uuid::from_u128(1)),
            conference_id: Some(Uuid::from_u128(2)),
            payload,
        }
    }

    #[test]
    fn destinations_parse_and_default_to_none() {
        assert_eq!(
            PublicEventsDestination::default(),
            PublicEventsDestination::None
        );
        for d in [
            PublicEventsDestination::None,
            PublicEventsDestination::DeveloperPlatform,
        ] {
            assert_eq!(PublicEventsDestination::parse(d.as_str()), Some(d));
        }
        assert_eq!(PublicEventsDestination::parse("calling"), None);
        assert_eq!(PublicEventsDestination::parse("both"), None);
    }

    #[test]
    fn only_lifecycle_facts_are_publishable() {
        for t in [
            "chat.message_created",
            "conference.created",
            "conference.ending",
            "participant.admitted",
            "rtc.room_terminate_requested",
            "transcription.ready",
            "meeting_space.updated",
        ] {
            assert!(public_type(t).is_none(), "{t}");
        }
        assert_eq!(
            public_type("conference.started"),
            Some("meeting-service.meeting.started")
        );
    }

    #[test]
    fn payloads_carry_ids_and_codes_only() {
        let pid = Uuid::new_v4();
        let (t, p) = public_payload(&record(
            "participant.joined",
            json!({ "participantId": pid, "role": "PARTICIPANT", "displayName": "Alice Guest",
                    "email": "alice@example.com", "participantSessionId": Uuid::new_v4(),
                    "mediaIdentity": "lk-ident", "rtcRoomName": "cf-abc" }),
        ))
        .unwrap();
        assert_eq!(t, "meeting-service.participant.joined");
        assert_eq!(
            p,
            json!({ "meeting_id": Uuid::from_u128(1), "conference_id": Uuid::from_u128(2),
                    "participant_id": pid, "role": "PARTICIPANT" })
        );
        let (_, p) = public_payload(&record(
            "artifact.ready",
            json!({ "artifactId": "not-a-uuid", "artifactType": "RECORDING", "status": "READY",
                    "durationMs": 1200, "storageKey": "recordings/a/b.mp4",
                    "downloadUrl": "https://s3.example/x", "sizeBytes": 5 }),
        ))
        .unwrap();
        let text = p.to_string();
        for leaked in ["recordings/", "https", "not-a-uuid", "sizeBytes"] {
            assert!(!text.contains(leaked), "{leaked}: {text}");
        }
        assert_eq!(p["artifact_kind"], "RECORDING");
        assert_eq!(p["duration_ms"], 1200);
        assert_eq!(p["artifact_id"], json!(Uuid::nil()));
        let (_, p) =
            public_payload(&record("conference.failed", json!({ "status": "FAILED" }))).unwrap();
        assert_eq!(p["ended_reason"], "FAILED");
        // Lower-case / free text is never a code.
        let (_, p) = public_payload(&record(
            "conference.started",
            json!({ "status": "live now" }),
        ))
        .unwrap();
        assert!(p.get("status").is_none());
    }
}
