//! Part F scenario helpers.
#![allow(dead_code)]

use axum::http::StatusCode;
use livekit_protocol as proto;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    livekit_mock,
    rtc::{connect, join_guest, join_user, lk, room, Joined},
    uuid, Auth, TestApp, User,
};

pub fn base(cid: Uuid) -> String {
    format!("/api/v2/conferences/{cid}")
}

/// A LIVE conference with one owner, plus `n` connected guests.
pub async fn meeting(app: &TestApp, body: Value, n: usize) -> (User, Uuid, Vec<Joined>) {
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, body).await;
    let mut guests = vec![];
    for i in 0..n {
        let g = join_guest(app, cid, &format!("guest-{i}")).await;
        connect(app, cid, &g).await;
        guests.push(g);
    }
    (owner, cid, guests)
}

pub fn public() -> Value {
    json!({ "title": "Part F", "accessPolicy": "PUBLIC" })
}

/// The owner joins as a participant (needed for host self-actions such as visits).
pub async fn owner_joined(app: &TestApp, cid: Uuid, owner: &User) -> Joined {
    let j = join_user(app, cid, owner).await;
    connect(app, cid, &j).await;
    j
}

/// Provider track webhook for a participant (signed like livekit-server).
pub async fn track_event(
    app: &TestApp,
    event: &str,
    room_name: &str,
    identity: &str,
    track_sid: &str,
    source: &str,
) -> (StatusCode, Value) {
    let body = livekit_mock::track_webhook_body(
        event,
        &format!("EV_{}", Uuid::new_v4().simple()),
        room_name,
        identity,
        track_sid,
        source,
    );
    app.signed_webhook(&body).await
}

/// Client publishes a screen share in its room and the provider reports it.
pub async fn start_share(app: &TestApp, cid: Uuid, j: &Joined) -> String {
    let sid = lk(app)
        .publish(&room(cid), &j.identity, proto::TrackSource::ScreenShare)
        .expect("share allowed");
    let (s, v) = track_event(
        app,
        "track_published",
        &room(cid),
        &j.identity,
        &sid,
        "SCREEN_SHARE",
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    sid
}

pub async fn policy(app: &TestApp, cid: Uuid, owner: &User, changes: Value) -> Value {
    let (_, st) = app
        .get(
            &format!("{}/collaboration", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let version = st["collaborationVersion"]
        .as_i64()
        .expect("collaboration state");
    let (s, v) = app
        .patch(
            &format!("{}/collaboration-policy", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "collaborationVersion": version, "changes": changes }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

/// Fresh media credential for a joined participant's session (current context).
pub async fn refresh(app: &TestApp, cid: Uuid, j: &Joined, auth: Auth<'_>) -> (StatusCode, Value) {
    app.post(
        &format!("{}/media-credential", base(cid)),
        auth,
        json!({ "participantSessionId": j.session_id }),
    )
    .await
}

pub async fn create_rooms(app: &TestApp, cid: Uuid, owner: &User, body: Value) -> Vec<Uuid> {
    let (s, v) = app
        .post(
            &format!("{}/breakouts", base(cid)),
            Auth::Bearer(&owner.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["breakouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| uuid(&b["id"]))
        .collect()
}

pub async fn open_room(app: &TestApp, cid: Uuid, owner: &User, bid: Uuid) -> Value {
    let (s, v) = app
        .post(
            &format!("{}/breakouts/{bid}/open", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

pub fn breakout_room(bid: Uuid) -> String {
    format!("cfbr-{}", bid.simple())
}

pub async fn send(app: &TestApp, cid: Uuid, auth: Auth<'_>, body: Value) -> (StatusCode, Value) {
    app.post(&format!("{}/messages", base(cid)), auth, body)
        .await
}

pub async fn messages(
    app: &TestApp,
    cid: Uuid,
    auth: Auth<'_>,
    query: &str,
) -> (StatusCode, Value) {
    app.get(&format!("{}/messages{query}", base(cid)), auth)
        .await
}

/// All committed client events of a type for a conference (payloads).
pub async fn event_payloads(app: &TestApp, cid: Uuid, event_type: &str) -> Vec<Value> {
    use sea_orm::ConnectionTrait;
    app.db
        .conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT payload::text FROM outbox_events WHERE conference_id='{cid}' AND event_type='{event_type}' ORDER BY conference_seq"
            ),
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_str(&r.try_get_by_index::<String>(0).unwrap()).unwrap())
        .collect()
}

pub async fn audits(app: &TestApp, cid: Uuid, action: &str) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='{action}'"
    ))
    .await
}

pub fn simple_poll(visibility: &str) -> Value {
    json!({
        "title": "Lunch?",
        "resultsVisibility": visibility,
        "questions": [{ "type": "SINGLE_CHOICE", "prompt": "Pizza or sushi?", "options": ["Pizza", "Sushi"] }],
    })
}

pub async fn open_poll(app: &TestApp, cid: Uuid, owner: &User, body: Value) -> Uuid {
    let (s, v) = app
        .post(
            &format!("{}/polls", base(cid)),
            Auth::Bearer(&owner.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let pid = uuid(&v["id"]);
    let (s, v) = app
        .post(
            &format!("{}/polls/{pid}/open", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    pid
}

pub async fn vote(
    app: &TestApp,
    cid: Uuid,
    poll: Uuid,
    auth: Auth<'_>,
    option: &str,
) -> (StatusCode, Value) {
    app.post(
        &format!("{}/polls/{poll}/responses", base(cid)),
        auth,
        json!({ "answers": [{ "questionId": "q1", "optionIds": [option] }] }),
    )
    .await
}
