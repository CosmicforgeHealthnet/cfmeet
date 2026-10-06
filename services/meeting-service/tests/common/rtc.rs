//! Part E scenario helpers (mock LiveKit provider).
#![allow(dead_code)]

use axum::http::StatusCode;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{livekit_mock, livekit_mock::MockLiveKit, uuid, Auth, TestApp, User};

pub fn room(cid: Uuid) -> String {
    format!("cf-{}", cid.simple())
}

pub fn lk(app: &TestApp) -> &MockLiveKit {
    app.lk.as_deref().expect("mock LiveKit app")
}

/// A joined participant with an issued media credential.
#[derive(Debug, Clone)]
pub struct Joined {
    pub participant_id: Uuid,
    pub session_id: Uuid,
    pub identity: String,
    pub credential: String,
    pub participant_token: Option<String>,
    pub body: Value,
}

impl Joined {
    pub fn auth(&self) -> Auth<'_> {
        Auth::Participant(self.participant_token.as_deref().expect("guest token"))
    }
}

fn joined(v: Value) -> Joined {
    assert_eq!(v["rtcStatus"], "ISSUED", "credential issued: {v}");
    let session_id = uuid(&v["participantSession"]["id"]);
    Joined {
        participant_id: uuid(&v["participant"]["id"]),
        session_id,
        identity: format!("ps_{}", session_id.simple()),
        credential: v["rtc"]["credential"].as_str().unwrap().to_string(),
        participant_token: v["participantToken"].as_str().map(str::to_string),
        body: v,
    }
}

pub async fn join_guest(app: &TestApp, cid: Uuid, name: &str) -> Joined {
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": name }))
        .await;
    assert_eq!(s, StatusCode::OK, "guest join: {v}");
    joined(v)
}

pub async fn join_user(app: &TestApp, cid: Uuid, user: &User) -> Joined {
    let (s, v) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "user join: {v}");
    joined(v)
}

/// Signed provider callback with a fresh event id.
pub async fn provider_event(
    app: &TestApp,
    event: &str,
    cid: Uuid,
    identity: Option<&str>,
) -> (StatusCode, Value) {
    let body = livekit_mock::webhook_body(
        event,
        &format!("EV_{}", Uuid::new_v4().simple()),
        &room(cid),
        None,
        identity,
    );
    app.signed_webhook(&body).await
}

/// Media connects at the provider (credential verified by the mock SFU) and the
/// provider reports `participant_joined`.
pub async fn connect(app: &TestApp, cid: Uuid, j: &Joined) {
    let identity = lk(app)
        .connect(&j.credential)
        .expect("SFU accepts credential");
    assert_eq!(identity, j.identity);
    let (s, v) = provider_event(app, "participant_joined", cid, Some(&identity)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "APPLIED", "{v}");
}

pub async fn events(app: &TestApp, cid: Uuid, event_type: &str) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='{event_type}'"
    ))
    .await
}

pub async fn session_state(app: &TestApp, session_id: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT state FROM participant_sessions WHERE id='{session_id}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

pub async fn conference_status(app: &TestApp, cid: Uuid) -> String {
    let (_, v) = app
        .get(&format!("/api/v2/conferences/{cid}"), Auth::None)
        .await;
    v["status"].as_str().unwrap_or_default().to_string()
}

pub async fn change_role(
    app: &TestApp,
    cid: Uuid,
    by: &User,
    pid: Uuid,
    role: &str,
) -> (StatusCode, Value) {
    app.patch(
        &format!("/api/v2/conferences/{cid}/participants/{pid}/role"),
        Auth::Bearer(&by.token),
        json!({ "role": role }),
    )
    .await
}

pub async fn exec(app: &TestApp, sql: &str) {
    use sea_orm::ConnectionTrait;
    app.db.conn.execute_unprepared(sql).await.unwrap();
}
