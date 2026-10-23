//! Part VI §53 — environment-scoped meeting events to the Developer Platform.
//!
//! The `developer-platform` destination is on for this test binary (staging is a process
//! setting); `tests/platform_access.rs` proves the default (`none`) stages nothing. A mock
//! Developer Platform verifies the REAL Meeting Service service token
//! (`developer-event.publish`), refuses a foreign producer and de-duplicates on the
//! event id. Envelopes are checked against `cfmeet.platform.event-envelope` v1 (required
//! keys, no additional properties, id / type / correlation patterns, payload key rules)
//! and carry only safe identifiers and lifecycle codes.

mod common;

use axum::http::{Method, StatusCode};
use backend::services::{
    meeting_events::{emit, OutboxRecord},
    platform::events::{self, PublicEventPublisher},
};
use cf_service_auth::ServiceId;
use common::{platform::*, receiver::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

const RW: &[&str] = &["meetings.read", "meetings.write"];

/// The Developer Platform's meeting allow-list (internal snake_case keys).
const ALLOWED_PAYLOAD_KEYS: &[&str] = &[
    "meeting_id",
    "conference_id",
    "participant_id",
    "recording_id",
    "artifact_id",
    "artifact_kind",
    "role",
    "status",
    "ended_reason",
    "duration_ms",
];

struct Setup {
    h: Harness,
    dp: Receiver,
    publisher: PublicEventPublisher,
}

async fn setup() -> Option<Setup> {
    events::set_staging(true);
    let h = Harness::new().await?;
    let key = signing_key("meeting-service-test-1");
    let dp = Receiver::developer_platform(&ServiceId::meeting_service(), &key, ENV).await;
    let client = producer_client(ServiceId::meeting_service(), key, ENV, &[&dp]);
    let publisher = PublicEventPublisher::new(h.app.db.conn.clone(), client, ENV);
    Some(Setup { h, dp, publisher })
}

macro_rules! setup {
    () => {
        match setup().await {
            Some(s) => s,
            None => return,
        }
    };
}

fn schema(rel: &str) -> Value {
    let path = format!(
        "{}/../../packages/contracts/{rel}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Structural validation against the envelope contract (and the common correlation id).
fn assert_envelope_conforms(e: &Value) {
    let envelope = schema("platform/events/envelope/v1.schema.json");
    let common = schema("platform/common/v1.schema.json");
    let obj = e.as_object().expect("object");
    for k in envelope["required"].as_array().unwrap() {
        assert!(obj.contains_key(k.as_str().unwrap()), "missing {k}: {e}");
    }
    let props = envelope["properties"].as_object().unwrap();
    for k in obj.keys() {
        assert!(props.contains_key(k), "additional property {k}");
    }
    let matches = |pattern: &Value, value: &str| {
        regex::Regex::new(pattern.as_str().unwrap())
            .unwrap()
            .is_match(value)
    };
    assert!(matches(
        &props["event_id"]["pattern"],
        e["event_id"].as_str().unwrap()
    ));
    assert!(matches(
        &props["event_type"]["pattern"],
        e["event_type"].as_str().unwrap()
    ));
    assert!(matches(
        &common["$defs"]["CorrelationId"]["pattern"],
        e["correlation_id"].as_str().unwrap()
    ));
    assert!(matches(
        &common["$defs"]["ServiceId"]["pattern"],
        e["producer_service"].as_str().unwrap()
    ));
    assert!(e["schema_version"].as_i64().unwrap() >= 1);
    chrono::DateTime::parse_from_rfc3339(e["occurred_at"].as_str().unwrap()).unwrap();
    let ownership = e["ownership"].as_object().unwrap();
    let allowed: Vec<&str> = props["ownership"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for (k, v) in ownership {
        assert!(allowed.contains(&k.as_str()), "{k}");
        Uuid::parse_str(v.as_str().unwrap()).unwrap();
    }
    let names = &props["payload"]["propertyNames"];
    for (k, v) in e["payload"].as_object().unwrap() {
        assert!(matches(&names["pattern"], k), "payload key {k}");
        assert!(!matches(&names["not"]["pattern"], k), "payload key {k}");
        assert!(
            ALLOWED_PAYLOAD_KEYS.contains(&k.as_str()),
            "payload key {k} outside the Developer Platform allow-list"
        );
        let safe = match v {
            Value::String(s) => {
                Uuid::parse_str(s).is_ok()
                    || (s.len() <= 64
                        && s.bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
            }
            Value::Number(n) => n.as_u64().is_some(),
            _ => false,
        };
        assert!(safe, "unsafe value {k}={v}");
    }
}

async fn staged(app: &TestApp) -> i64 {
    app.scalar("SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT'")
        .await
}

#[tokio::test]
async fn environment_meetings_publish_safe_envelopes_to_the_developer_platform() {
    let Setup { h, dp, publisher } = setup!();
    let m = Machine::new(RW);
    h.register(&m);
    let (s, v, correlation) = h
        .forwarded_by(
            &h.gateway,
            Method::POST,
            "/api/v2/instant-meetings",
            Some(json!({ "title": "Board meeting about Alice", "accessPolicy": "PUBLIC" })),
            Some(m.delegated()),
            &[],
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let space = uuid(&v["meetingSpace"]["id"]);
    let conference = uuid(&v["conference"]["id"]);
    let code = v["meetingSpace"]["meetingCode"]
        .as_str()
        .unwrap()
        .to_string();
    // meeting.created + meeting.started, committed with the facts.
    assert_eq!(staged(&h.app).await, 2);
    // A CF Meet user's meeting never produces a platform event.
    let user = h.app.user("someone").await;
    let (s, _) = h
        .app
        .post(
            "/api/v2/instant-meetings",
            Auth::Bearer(&user.token),
            json!({ "title": "private" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(staged(&h.app).await, 2);
    // Lifecycle facts produced by provider callbacks / workers (here written directly):
    // whatever the internal payload holds, only allow-listed ids and codes are staged.
    let guest = Uuid::new_v4();
    for (event_type, payload) in [
        (
            "participant.joined",
            json!({ "participantId": guest, "role": "PARTICIPANT", "displayName": "Alice Guest",
                    "email": "alice@example.com", "participantSessionId": Uuid::new_v4(),
                    "mediaIdentity": "lk_PA_secret", "rtcRoomName": "cf-room" }),
        ),
        (
            "participant.left",
            json!({ "participantId": guest, "role": "PARTICIPANT", "sessionIds": [Uuid::new_v4()] }),
        ),
        (
            "recording.ready",
            json!({ "recordingId": Uuid::new_v4(), "status": "READY",
                    "storageKey": "recordings/x/y.mp4", "downloadUrl": "https://s3.example/x" }),
        ),
        (
            "conference.ended",
            json!({ "conferenceId": conference, "status": "ENDED", "rtcRoomId": "RM_abc" }),
        ),
    ] {
        emit(
            &h.app.db.conn,
            OutboxRecord {
                aggregate_type: "test",
                aggregate_id: Uuid::new_v4(),
                event_type,
                meeting_space_id: Some(space),
                conference_id: Some(conference),
                payload,
            },
        )
        .await
        .unwrap();
    }
    // Internal facts (chat, admission, provider commands) are never staged.
    for event_type in [
        "chat.message_created",
        "participant.admitted",
        "rtc.room_terminate_requested",
    ] {
        emit(
            &h.app.db.conn,
            OutboxRecord {
                aggregate_type: "test",
                aggregate_id: Uuid::new_v4(),
                event_type,
                meeting_space_id: Some(space),
                conference_id: Some(conference),
                payload: json!({ "text": "hello" }),
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(staged(&h.app).await, 6);

    let report = publisher.run_once().await.unwrap();
    assert_eq!(report.published, 6, "{report:?}");
    let accepted = dp.accepted();
    assert_eq!(accepted.len(), 6);
    assert_eq!(dp.unauthenticated(), 0);
    let types: Vec<&str> = accepted
        .iter()
        .map(|e| e["event_type"].as_str().unwrap())
        .collect();
    for t in [
        "meeting-service.meeting.created",
        "meeting-service.meeting.started",
        "meeting-service.participant.joined",
        "meeting-service.participant.left",
        "meeting-service.recording.ready",
        "meeting-service.meeting.ended",
    ] {
        assert!(types.contains(&t), "{t} in {types:?}");
    }
    let a = &m.actor;
    for e in &accepted {
        assert_envelope_conforms(e);
        assert_eq!(e["producer_service"], "cfmeet.meeting-service");
        assert_eq!(e["runtime_environment"], "test");
        assert_eq!(e["schema_version"], 1);
        assert_eq!(
            e["ownership"],
            json!({ "tenant_id": a.tenant_id, "application_id": a.application_id,
                    "application_environment_id": a.environment_id })
        );
        assert_eq!(e["payload"]["meeting_id"], json!(space));
        let text = e.to_string();
        for leaked in [
            "Alice",
            "alice@example.com",
            "Board meeting",
            code.as_str(),
            "lk_PA_secret",
            "cf-room",
            "recordings/",
            "https://",
            "RM_abc",
            "hello",
        ] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
    }
    // The forwarded request's signed correlation id is carried into its events.
    for t in [
        "meeting-service.meeting.created",
        "meeting-service.meeting.started",
    ] {
        let e = accepted.iter().find(|e| e["event_type"] == t).unwrap();
        assert_eq!(e["correlation_id"], correlation.as_str(), "{t}");
    }
    let joined = accepted
        .iter()
        .find(|e| e["event_type"] == "meeting-service.participant.joined")
        .unwrap();
    assert_eq!(
        joined["payload"],
        json!({ "meeting_id": space, "conference_id": conference,
                "participant_id": guest, "role": "PARTICIPANT" })
    );
    // The hop to the Developer Platform continues the committed correlation id.
    for (envelope, header) in dp.attempts() {
        assert_eq!(envelope["correlation_id"].as_str().unwrap(), header);
    }
    // Stable source ids: nothing is published twice.
    let report = publisher.run_once().await.unwrap();
    assert_eq!(report.published, 0);
    assert_eq!(dp.accepted().len(), 6);
    assert_eq!(
        h.app
            .scalar("SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT' AND status = 'PUBLISHED'")
            .await,
        6
    );
    // Realtime clients never see the channel (the replay cursor counts REALTIME only).
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT' AND conference_seq IS NOT NULL AND conference_id = '{conference}'"
            ))
            .await,
        0
    );
}

#[tokio::test]
async fn refusals_dead_letter_and_outages_retry_under_the_same_event_id() {
    let Setup { h, dp, publisher } = setup!();
    let m = Machine::new(RW);
    h.register(&m);
    let (s, v) = h
        .machine(
            &m,
            Method::POST,
            "/api/v2/instant-meetings",
            Some(json!({ "title": "x" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(staged(&h.app).await, 2);
    dp.script(&[(503, "SERVICE_UNAVAILABLE"), (400, "VALIDATION_ERROR")]);
    let report = publisher.run_once().await.unwrap();
    assert_eq!((report.retried, report.dead_lettered), (1, 1), "{report:?}");
    assert_eq!(
        h.app
            .scalar("SELECT count(*) FROM outbox_events WHERE channel = 'PUBLIC_EVENT' AND status = 'FAILED' AND last_error LIKE 'REFUSED VALIDATION_ERROR%'")
            .await,
        1
    );
    let retried_id: String = {
        use sea_orm::{ConnectionTrait, Statement};
        let row = h
            .app
            .db
            .conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT id::text FROM outbox_events WHERE channel = 'PUBLIC_EVENT' AND status = 'PENDING' AND attempts = 1 AND available_at > now()".to_string(),
            ))
            .await
            .unwrap()
            .expect("retried row backs off");
        row.try_get_by_index(0).unwrap()
    };
    // Due again: published under the same source event id.
    h.app
        .scalar("WITH u AS (UPDATE outbox_events SET available_at = now() WHERE channel = 'PUBLIC_EVENT' AND status = 'PENDING' RETURNING 1) SELECT count(*) FROM u")
        .await;
    let report = publisher.run_once().await.unwrap();
    assert_eq!(report.published, 1, "{report:?}");
    let accepted = dp.accepted();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0]["event_id"],
        format!("evt_{}", Uuid::parse_str(&retried_id).unwrap().simple())
    );
}

#[tokio::test]
async fn concurrent_publishers_deliver_each_event_once() {
    let Setup { h, dp, publisher } = setup!();
    let key = signing_key("meeting-service-test-2");
    let dp2 = Receiver::developer_platform(&ServiceId::meeting_service(), &key, ENV).await;
    let second = PublicEventPublisher::new(
        h.app.db.conn.clone(),
        producer_client(ServiceId::meeting_service(), key, ENV, &[&dp2]),
        ENV,
    );
    let m = Machine::new(RW);
    h.register(&m);
    for i in 0..5 {
        let (s, v) = h
            .machine(
                &m,
                Method::POST,
                "/api/v2/instant-meetings",
                Some(json!({ "title": format!("m{i}") })),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    assert_eq!(staged(&h.app).await, 10);
    let (a, b) = tokio::join!(publisher.run_once(), second.run_once());
    assert_eq!(a.unwrap().published + b.unwrap().published, 10);
    let mut ids: Vec<String> = dp
        .accepted()
        .into_iter()
        .chain(dp2.accepted())
        .map(|e| e["event_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 10);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 10, "no event published by both instances");
}
