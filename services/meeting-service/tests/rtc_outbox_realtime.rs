//! Phase 2 Part E — durable outbox, Redis fanout and WebSocket realtime (spec §11–§16).

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use backend::{
    config::FeatureFlags,
    models::outbox_events,
    services::{realtime::RedisPublisher, rtc_control::RtcControl},
    workers::outbox_relay::{EventPublisher, OutboxRelay},
};
use common::{rtc::*, *};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::livekit().await {
            Some(a) => a,
            None => return,
        }
    };
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws(app: &TestApp, addr: std::net::SocketAddr, cid: Uuid, auth: Auth<'_>) -> Ws {
    let (s, t) = app
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            auth,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let url = format!(
        "ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={}",
        t["ticket"].as_str().unwrap()
    );
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

async fn frame(ws: &mut Ws) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .ok()??
        {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(c)) => {
                return Some(json!({ "type": "__close", "code": c.map(|c| u16::from(c.code)) }))
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

async fn until(ws: &mut Ws, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut seen = vec![];
    while let Some(f) = frame(ws).await {
        let done = pred(&f);
        seen.push(f);
        if done {
            return seen;
        }
    }
    panic!("condition never met; frames: {seen:?}");
}

/// O1. Events commit with the state change, with a gap-free per-conference sequence;
/// rejected operations write nothing; provider commands carry no client sequence.
#[tokio::test]
async fn o1_transactional_gap_free_sequence() {
    let app = app!();
    let owner = app.user("owner").await;
    let stranger = app.user("stranger").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o1", "accessPolicy": "PUBLIC" }))
        .await;
    for _ in 0..3 {
        join_guest(&app, cid, "g").await;
    }
    app.post(
        &format!("/api/v2/conferences/{cid}/lock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let before = app.scalar("SELECT count(*) FROM outbox_events").await;
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/unlock"),
            Auth::Bearer(&stranger.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events").await,
        before,
        "rejected op wrote nothing"
    );
    let n = app
        .scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME'"
        ))
        .await;
    assert!(n >= 7);
    assert_eq!(app.scalar(&format!("SELECT count(DISTINCT conference_seq) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND conference_seq BETWEEN 1 AND {n}")).await, n, "exactly 1..N");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND event_seq={n}"
        ))
        .await,
        1
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type LIKE 'rtc.%' AND (channel <> 'PROVIDER_COMMAND' OR conference_seq IS NOT NULL)")).await, 0);
}

struct Recorder(Mutex<Vec<String>>);

#[async_trait]
impl EventPublisher for Recorder {
    async fn publish(&self, e: &outbox_events::Model) -> Result<(), String> {
        self.0.lock().unwrap().push(e.event_type.clone());
        Ok(())
    }
}

/// O2. Provider commands and client events stay logically distinct.
#[tokio::test]
async fn o2_channel_separation() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o2", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "g").await;
    connect(&app, cid, &g).await;
    // A command left pending (as if the request path crashed before dispatch).
    exec(&app, &format!("INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, payload, conference_id, channel) VALUES (gen_random_uuid(), 'conference', '{cid}', 'rtc.permissions_update_requested', '{{\"conferenceId\":\"{cid}\",\"participantId\":\"{}\"}}', '{cid}', 'PROVIDER_COMMAND')", g.participant_id)).await;
    let rec = Arc::new(Recorder(Mutex::new(vec![])));
    let relay = OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(rec.clone() as Arc<dyn EventPublisher>),
        None,
    );
    relay.tick(1000).await.unwrap();
    let published = rec.0.lock().unwrap().clone();
    assert!(!published.is_empty());
    assert!(
        published.iter().all(|t| !t.starts_with("rtc.")),
        "client publisher never sees commands: {published:?}"
    );
    assert_eq!(app.scalar("SELECT count(*) FROM outbox_events WHERE channel='PROVIDER_COMMAND' AND status='PENDING'").await, 1, "commands wait for the control plane");
    // The control plane executes it.
    let control = OutboxRelay::with_control(
        app.db.conn.clone(),
        None,
        Some(Arc::new(RtcControl::new(app.state.meeting.core.clone()))),
    );
    control.tick(1000).await.unwrap();
    assert_eq!(app.scalar("SELECT count(*) FROM outbox_events WHERE channel='PROVIDER_COMMAND' AND status='PENDING'").await, 0);
    // The Redis publisher refuses commands outright.
    let cmd = {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        outbox_events::Entity::find()
            .filter(outbox_events::Column::EventType.like("rtc.%"))
            .one(&app.db.conn)
            .await
            .unwrap()
            .unwrap()
    };
    assert!(RedisPublisher::new(app.state.redis.clone())
        .publish(&cmd)
        .await
        .is_err());
}

/// O3. Redis fanout delivers the stable, scrubbed envelope.
#[tokio::test]
async fn o3_redis_fanout_envelope() {
    if !require_test_redis() {
        return;
    }
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o3", "accessPolicy": "PUBLIC" }))
        .await;
    let mut sub = app
        .state
        .redis
        .subscribe(&backend::services::realtime::conference_channel(cid))
        .await
        .unwrap()
        .into_on_message();
    let g = join_guest(&app, cid, "g").await;
    connect(&app, cid, &g).await;
    let relay = OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(RedisPublisher::new(app.state.redis.clone())) as Arc<dyn EventPublisher>),
        None,
    );
    relay.tick(1000).await.unwrap();
    let mut got = vec![];
    while let Ok(Some(msg)) = tokio::time::timeout(Duration::from_millis(500), sub.next()).await {
        let v: Value = serde_json::from_str(&msg.get_payload::<String>().unwrap()).unwrap();
        got.push(v);
    }
    let types: Vec<_> = got
        .iter()
        .map(|v| v["eventType"].as_str().unwrap().to_string())
        .collect();
    assert!(
        types.contains(&"participant.joined".to_string()),
        "{types:?}"
    );
    for v in &got {
        for k in [
            "eventId",
            "eventType",
            "schemaVersion",
            "conferenceId",
            "meetingSpaceId",
            "occurredAt",
            "payload",
            "sequence",
        ] {
            assert!(v.get(k).is_some(), "envelope field {k}: {v}");
        }
        let s = v.to_string();
        assert!(
            !s.contains("mediaIdentity") && !s.contains("ps_") && !s.contains("rtc."),
            "{s}"
        );
    }
    let seqs: Vec<i64> = got.iter().filter_map(|v| v["sequence"].as_i64()).collect();
    let mut sorted = seqs.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), seqs.len(), "no duplicate sequence");
}

/// O4. Poison events are dead-lettered without blocking healthy ones.
#[tokio::test]
async fn o4_poison_tolerance() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o4", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, other) = app
        .live_conference(
            &owner,
            json!({ "title": "other", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let foreign = join_guest(&app, other, "foreign").await;
    let bad = [
        (
            "rtc.bogus",
            format!("{{\"conferenceId\":\"{cid}\"}}"),
            "PROVIDER_COMMAND",
        ),
        (
            "rtc.participant_remove_requested",
            "{}".to_string(),
            "PROVIDER_COMMAND",
        ),
        (
            "rtc.participant_remove_requested",
            format!(
                "{{\"conferenceId\":\"{cid}\",\"participantSessionId\":\"{}\"}}",
                foreign.session_id
            ),
            "PROVIDER_COMMAND",
        ),
        ("rtc.sneaky", "{}".to_string(), "REALTIME"),
    ];
    for (t, payload, channel) in &bad {
        let conf = if payload == "{}" && *t == "rtc.participant_remove_requested" {
            "NULL".to_string()
        } else {
            format!("'{cid}'")
        };
        exec(&app, &format!("INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, payload, conference_id, channel) VALUES (gen_random_uuid(), 'x', gen_random_uuid(), '{t}', '{payload}', {conf}, '{channel}')")).await;
    }
    let relay = OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(Recorder(Mutex::new(vec![]))) as Arc<dyn EventPublisher>),
        Some(Arc::new(RtcControl::new(app.state.meeting.core.clone()))),
    );
    relay.tick(1000).await.unwrap();
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='FAILED'")
            .await,
        4,
        "all poison rows dead-lettered"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING'")
            .await,
        0,
        "healthy rows delivered in the same pass"
    );
    assert_eq!(
        lk(&app).call_count("RemoveParticipant"),
        0,
        "a command can never reach another conference's session"
    );
}

/// O5. Waiting participants get a SELF_ONLY stream; it widens after admission.
#[tokio::test]
async fn o5_scoped_subscription() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "o5", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let addr = app.serve().await;
    let (_, me) = app
        .join(cid, Auth::None, json!({ "displayName": "me" }))
        .await;
    let (_, other) = app
        .join(cid, Auth::None, json!({ "displayName": "other" }))
        .await;
    let my_token = me["participantToken"].as_str().unwrap().to_string();
    let (my_id, other_id) = (
        uuid(&me["participant"]["id"]),
        uuid(&other["participant"]["id"]),
    );
    let mut s = ws(&app, addr, cid, Auth::Participant(&my_token)).await;
    let hello = frame(&mut s).await.unwrap();
    assert_eq!(hello["scope"], "SELF_ONLY");
    app.post(
        &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
        Auth::Bearer(&owner.token),
        json!({ "participantIds": [other_id] }),
    )
    .await;
    app.post(
        &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
        Auth::Bearer(&owner.token),
        json!({ "participantIds": [my_id] }),
    )
    .await;
    let frames = until(&mut s, |f| {
        f["type"] == "event" && f["event"]["payload"]["participantId"] == json!(my_id)
    })
    .await;
    assert!(
        frames
            .iter()
            .all(|f| f["event"]["payload"]["participantId"] != json!(other_id)),
        "no foreign participant events while waiting"
    );
    // Now FULL: other participants' events flow.
    app.post(
        &format!("/api/v2/conferences/{cid}/participants/{other_id}/remove"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    until(&mut s, |f| {
        f["event"]["eventType"] == "participant.removed"
            && f["event"]["payload"]["participantId"] == json!(other_id)
    })
    .await;
}

/// O6. A subscriber removed from the conference is revoked (4403).
#[tokio::test]
async fn o6_revocation_on_removal() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o6", "accessPolicy": "PUBLIC" }))
        .await;
    let addr = app.serve().await;
    let g = join_guest(&app, cid, "g").await;
    let mut s = ws(&app, addr, cid, g.auth()).await;
    frame(&mut s).await.unwrap();
    app.post(
        &format!(
            "/api/v2/conferences/{cid}/participants/{}/remove",
            g.participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let frames = until(&mut s, |f| f["type"] == "__close").await;
    assert!(frames.iter().any(|f| f["type"] == "revoked"), "{frames:?}");
    assert_eq!(frames.last().unwrap()["code"], 4403);
    // And cannot resubscribe.
    let (st, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            g.auth(),
            json!({}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

/// O7. Conference end is delivered, then the stream closes; header auth works for
/// non-browser clients.
#[tokio::test]
async fn o7_end_closes_stream_header_auth() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o7", "accessPolicy": "PUBLIC" }))
        .await;
    let addr = app.serve().await;
    let mut req = format!("ws://{addr}/api/v2/conferences/{cid}/realtime")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", owner.token).parse().unwrap(),
    );
    let (mut s, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(frame(&mut s).await.unwrap()["type"], "subscribed");
    app.post(
        &format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let frames = until(&mut s, |f| f["type"] == "__close").await;
    let types: Vec<_> = frames
        .iter()
        .map(|f| {
            f["event"]["eventType"]
                .as_str()
                .unwrap_or(f["type"].as_str().unwrap())
                .to_string()
        })
        .collect();
    assert!(
        types.contains(&"conference.ending".to_string())
            && types.contains(&"conference.ended".to_string()),
        "{types:?}"
    );
    assert!(types.contains(&"closed".to_string()));
}

/// O8. With `conference_realtime_enabled` off, realtime endpoints are unavailable.
#[tokio::test]
async fn o8_realtime_flag() {
    let Some(app) = TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: FeatureFlags {
            rtc_provider_control_v2: true,
            ..FeatureFlags::all_v2_enabled()
        },
        ..TestOptions::default()
    })
    .await
    else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "o8", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "FEATURE_DISABLED"));
    let (s, _) = app
        .get(
            &format!("/api/v2/conferences/{cid}/events"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// A realtime ticket is one-shot: it opens exactly one subscription; a replay (another
/// client, or a reconnect reusing it) is refused before the upgrade like an invalid
/// ticket, and a fresh ticket works.
#[tokio::test]
async fn realtime_tickets_are_single_use() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "one-shot", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let addr = app.serve().await;
    let (s, t) = app
        .post(
            &format!("/api/v2/conferences/{cid}/realtime/ticket"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    let url = format!(
        "ws://{addr}/api/v2/conferences/{cid}/realtime?ticket={}",
        t["ticket"].as_str().unwrap()
    );
    let (mut first, _) = tokio_tungstenite::connect_async(url.clone()).await.unwrap();
    assert_eq!(frame(&mut first).await.unwrap()["type"], "subscribed");
    match tokio_tungstenite::connect_async(url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED)
        }
        Err(e) => panic!("unexpected error {e:?}"),
        Ok(_) => panic!("a replayed realtime ticket opened a second subscription"),
    }
    let mut again = ws(&app, addr, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(frame(&mut again).await.unwrap()["type"], "subscribed");
}
