//! Permanent Phase 2 concurrency suite (spec §9). Requests race for real on a
//! multi-threaded runtime against PostgreSQL.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use backend::domain::Role;
use chrono::{Duration, Utc};
use common::*;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
}

fn spawn(
    router: &Router,
    method: Method,
    uri: String,
    header: Option<(&'static str, String)>,
    body: Value,
) -> JoinHandle<(StatusCode, Value)> {
    let router = router.clone();
    tokio::spawn(async move {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some((k, v)) = header {
            req = req.header(k, v);
        }
        let resp = router
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    })
}

fn bearer(u: &User) -> Option<(&'static str, String)> {
    Some(("authorization", format!("Bearer {}", u.token)))
}

async fn collect(handles: Vec<JoinHandle<(StatusCode, Value)>>) -> Vec<(StatusCode, Value)> {
    let mut out = vec![];
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

/// Two (many) simultaneous conference starts → one canonical LIVE state.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c01_double_start() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app.create_space(&owner, json!({ "title": "x" })).await;
    let sid = uuid(&space["id"]);
    // Two different occurrences of the same space raced: only one may be LIVE.
    let (_, a) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let (_, b) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let mut handles = vec![];
    for i in 0..10 {
        let c = if i % 2 == 0 { &a } else { &b };
        handles.push(spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/conferences/{}/start", c["id"].as_str().unwrap()),
            bearer(&owner),
            json!({}),
        ));
    }
    let results = collect(handles).await;
    let changed = results
        .iter()
        .filter(|(s, v)| *s == StatusCode::OK && v["changed"] == true)
        .count();
    assert_eq!(changed, 1, "{results:?}");
    for (s, v) in &results {
        assert!(
            *s == StatusCode::OK || code(v) == "CONFERENCE_STATE_CONFLICT",
            "{s} {v}"
        );
    }
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='LIVE'")).await, 1);
}

/// Two simultaneous admit requests → one final admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c02_double_admit() {
    let app = app!();
    let owner = app.user("owner").await;
    let cohost = app.user("cohost").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    app.add_member(sid, &owner, &cohost, Role::CoHost).await;
    let (_, g) = app
        .join(cid, Auth::None, json!({ "displayName": "g" }))
        .await;
    let pid = g["participant"]["id"].clone();
    let mut handles = vec![];
    for i in 0..10 {
        let who = if i % 2 == 0 { &owner } else { &cohost };
        handles.push(spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            bearer(who),
            json!({ "participantIds": [pid] }),
        ));
    }
    let results = collect(handles).await;
    let changed: usize = results
        .iter()
        .map(|(_, v)| v["changed"].as_array().map(|a| a.len()).unwrap_or(0))
        .sum();
    assert_eq!(changed, 1, "{results:?}");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='participant.admitted'")).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='ADMIT'")).await, 1);
}

/// Role change racing removal → deterministic terminal state (REMOVED).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c03_role_change_vs_remove() {
    let app = app!();
    let owner = app.user("owner").await;
    let cohost = app.user("cohost").await;
    for round in 0..5 {
        let (sid, cid) = app
            .live_conference(
                &owner,
                json!({ "title": format!("r{round}"), "accessPolicy": "PUBLIC" }),
            )
            .await;
        app.add_member(sid, &owner, &cohost, Role::CoHost).await;
        let (_, g) = app
            .join(cid, Auth::None, json!({ "displayName": "t" }))
            .await;
        let pid = g["participant"]["id"].as_str().unwrap().to_string();
        let role = spawn(
            &app.router,
            Method::PATCH,
            format!("/api/v2/conferences/{cid}/participants/{pid}/role"),
            bearer(&cohost),
            json!({ "role": "PRESENTER" }),
        );
        let remove = spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/conferences/{cid}/participants/{pid}/remove"),
            bearer(&owner),
            json!({}),
        );
        let (rs, rv) = role.await.unwrap();
        let (ms, _) = remove.await.unwrap();
        assert_eq!(ms, StatusCode::OK);
        assert!(
            rs == StatusCode::OK || code(&rv) == "PARTICIPANT_STATE_CONFLICT",
            "{rs} {rv}"
        );
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND admission_state='REMOVED'")).await, 1);
        app.post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    }
}

/// End racing join → fail closed: no active session established after ENDING began.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c04_end_vs_join() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    let mut handles = vec![];
    for i in 0..20 {
        handles.push(spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            None,
            json!({ "displayName": format!("g{i}") }),
        ));
        if i == 8 {
            handles.push(spawn(
                &app.router,
                Method::POST,
                format!("/api/v2/conferences/{cid}/end"),
                bearer(&owner),
                json!({}),
            ));
        }
    }
    let results = collect(handles).await;
    for (s, v) in &results {
        assert!(
            *s == StatusCode::OK || code(v) == "CONFERENCE_NOT_LIVE",
            "{s} {v}"
        );
    }
    let late_sessions = app
        .scalar(&format!(
            "SELECT count(*) FROM participant_sessions ps JOIN conference_sessions c ON c.id = ps.conference_id \
             WHERE c.id='{cid}' AND ps.established_at > c.ending_requested_at AND ps.state IN ('PENDING','CONNECTED')"
        ))
        .await;
    assert_eq!(late_sessions, 0);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND status='ENDING'"
        ))
        .await,
        1
    );
}

/// Multiple scheduler instances → no duplicate occurrences.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c05_parallel_schedulers() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(&owner, json!({ "title": "x", "timezone": "Europe/London" }))
        .await;
    let sid = uuid(&space["id"]);
    let start = (Utc::now() + Duration::days(1))
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let (s, _) = app
        .call(
            Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": "FREQ=DAILY", "startLocal": start, "durationMinutes": 30 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let initial = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await;
    // Force every rule due again, then race several scheduler instances.
    app.db
        .conn
        .execute_unprepared("UPDATE meeting_recurrence_rules SET materialized_until = NULL")
        .await
        .unwrap();
    use sea_orm::ConnectionTrait;
    let mut handles = vec![];
    for _ in 0..6 {
        let meeting = app.state.meeting.clone();
        handles.push(tokio::spawn(async move {
            backend::workers::ConferenceScheduler::new(meeting)
                .tick(Utc::now() + Duration::days(3))
                .await
                .unwrap()
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let total = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await;
    let distinct = app.scalar(&format!("SELECT count(DISTINCT occurrence_key) FROM conference_sessions WHERE meeting_space_id='{sid}'")).await;
    assert_eq!(total, distinct, "no duplicate occurrence keys");
    assert!(
        total >= initial && total <= initial + 3,
        "bounded: {initial} → {total}"
    );
    assert!(total <= 50, "bounded per run");
}

/// Concurrent invitation use → use_limit can never be exceeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c06_invitation_use_race() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let (_, inv) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "useLimit": 3 }),
        )
        .await;
    let token = inv["token"].as_str().unwrap().to_string();
    let mut handles = vec![];
    for i in 0..15 {
        handles.push(spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            None,
            json!({ "displayName": format!("g{i}"), "invitationToken": token }),
        ));
    }
    let results = collect(handles).await;
    let ok = results.iter().filter(|(s, _)| *s == StatusCode::OK).count();
    assert_eq!(ok, 3, "{results:?}");
    assert!(results
        .iter()
        .all(|(s, v)| *s == StatusCode::OK || code(v) == "INVITATION_INVALID"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT usage_count::bigint FROM meeting_invitations WHERE id='{}'",
            inv["invitation"]["id"].as_str().unwrap()
        ))
        .await,
        3
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        3,
        "failed uses leave no participant"
    );
}

/// Concurrent meeting policy updates → exactly one wins, stale versions rejected.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c07_policy_version_race() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app.create_space(&owner, json!({ "title": "x" })).await;
    let sid = uuid(&space["id"]);
    let mut handles = vec![];
    for i in 0..10 {
        handles.push(spawn(
            &app.router,
            Method::PATCH,
            format!("/api/v2/meeting-spaces/{sid}"),
            bearer(&owner),
            json!({ "version": 1, "title": format!("t{i}") }),
        ));
    }
    let results = collect(handles).await;
    assert_eq!(
        results.iter().filter(|(s, _)| *s == StatusCode::OK).count(),
        1
    );
    assert!(results
        .iter()
        .all(|(s, v)| *s == StatusCode::OK || code(v) == "MEETING_VERSION_CONFLICT"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT version::bigint FROM meeting_spaces WHERE id='{sid}'"
        ))
        .await,
        2
    );
}

/// Registered user joining simultaneously → one logical participant.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c08_registered_join_race() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let mut handles = vec![];
    for _ in 0..10 {
        handles.push(spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            bearer(&user),
            json!({}),
        ));
    }
    let results = collect(handles).await;
    let ok: Vec<_> = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::OK)
        .collect();
    assert!(!ok.is_empty());
    let ids: std::collections::HashSet<_> = ok
        .iter()
        .map(|(_, v)| v["participant"]["id"].clone())
        .collect();
    assert_eq!(ids.len(), 1);
    assert!(
        results
            .iter()
            .all(|(s, v)| *s == StatusCode::OK || code(v) == "PARTICIPANT_STATE_CONFLICT"),
        "only the device cap may refuse"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND user_id='{}'", user.id)).await, 1);
    assert!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state='PENDING'")).await <= 5);
}

/// Concurrent membership adds for the same principal → one active membership.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn c09_membership_race() {
    let app = app!();
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let space = app.create_space(&owner, json!({ "title": "x" })).await;
    let sid = uuid(&space["id"]);
    let mut handles = vec![];
    for _ in 0..8 {
        handles.push(spawn(
            &app.router,
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/members"),
            bearer(&owner),
            json!({ "userId": member.id, "role": "PARTICIPANT" }),
        ));
    }
    let results = collect(handles).await;
    assert_eq!(
        results
            .iter()
            .filter(|(s, _)| *s == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_members WHERE meeting_space_id='{sid}' AND status='ACTIVE'")).await, 1);
    let _ = Uuid::nil();
}
