//! Phase 2 Part E — concurrency suite (spec §18). Real PostgreSQL row locks, real
//! LiveKitProvider over HTTP to the mock RoomService.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use backend::{
    models::outbox_events,
    services::rtc_control::RtcControl,
    workers::outbox_relay::{EventPublisher, OutboxRelay},
};
use common::{livekit_mock, rtc::*, *};
use futures_util::future::join_all;
use serde_json::json;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::livekit().await {
            Some(a) => a,
            None => return,
        }
    };
}

async fn created(app: &TestApp, owner: &User) -> Uuid {
    let space = app
        .create_space(owner, json!({ "title": "c", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{}/conferences", uuid(&space["id"])),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    uuid(&c["id"])
}

/// C1. Double room creation: concurrent ensure_room → one provider room, one READY.
#[tokio::test]
async fn c1_double_room_creation() {
    let app = app!();
    let owner = app.user("owner").await;
    let cid = created(&app, &owner).await;
    // Move to PREPARING without letting the request path create the room.
    exec(
        &app,
        &format!("UPDATE conference_sessions SET status='PREPARING' WHERE id='{cid}'"),
    )
    .await;
    let control = Arc::new(RtcControl::new(app.state.meeting.core.clone()));
    let results = join_all((0..8).map(|_| {
        let c = control.clone();
        async move { c.ensure_room(cid).await }
    }))
    .await;
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
    assert_eq!(lk(&app).room_names(), vec![room(cid)]);
    assert_eq!(events(&app, cid, "conference.ready").await, 1);
    assert_eq!(conference_status(&app, cid).await, "WAITING");
}

/// C2. Parallel PREPARE: exactly one PREPARING transition, one READY, one room.
#[tokio::test]
async fn c2_parallel_prepare() {
    let app = app!();
    let owner = app.user("owner").await;
    let cid = created(&app, &owner).await;
    let uri = format!("/api/v2/conferences/{cid}/prepare");
    let results =
        join_all((0..8).map(|_| app.post(&uri, Auth::Bearer(&owner.token), json!({})))).await;
    assert!(
        results.iter().all(|(s, _)| *s == StatusCode::OK),
        "{results:?}"
    );
    assert!(results
        .iter()
        .any(|(_, v)| v["changed"] == true && v["conference"]["status"] == "WAITING"));
    assert_eq!(events(&app, cid, "conference.preparing").await, 1);
    assert_eq!(events(&app, cid, "conference.ready").await, 1);
    assert_eq!(lk(&app).room_names().len(), 1);
    assert_eq!(conference_status(&app, cid).await, "WAITING");
}

/// C3. Double END: one ENDING transition, ENDED once, one conference.ended.
#[tokio::test]
async fn c3_double_end() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c3", "accessPolicy": "PUBLIC" }))
        .await;
    let a = join_guest(&app, cid, "a").await;
    connect(&app, cid, &a).await;
    let uri = format!("/api/v2/conferences/{cid}/end");
    let results =
        join_all((0..6).map(|_| app.post(&uri, Auth::Bearer(&owner.token), json!({})))).await;
    assert!(
        results.iter().all(|(s, _)| *s == StatusCode::OK),
        "{results:?}"
    );
    assert_eq!(
        results.iter().filter(|(_, v)| v["changed"] == true).count(),
        1
    );
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(events(&app, cid, "conference.ending").await, 1);
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
    assert!(!lk(&app).has_room(&room(cid)));
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='CONFERENCE_END_COMPLETE'")).await, 1);
}

/// C4. END racing JOIN: no credential for a session established after ENDING, and no
/// active session or provider participant survives ENDED.
#[tokio::test]
async fn c4_end_racing_join() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c4", "accessPolicy": "PUBLIC" }))
        .await;
    let end_uri = format!("/api/v2/conferences/{cid}/end");
    let end = app.post(&end_uri, Auth::Bearer(&owner.token), json!({}));
    let joins = join_all(
        (0..12).map(|i| app.join(cid, Auth::None, json!({ "displayName": format!("g{i}") }))),
    );
    let ((s, _), joins) = tokio::join!(end, joins);
    assert_eq!(s, StatusCode::OK);
    for (s, v) in &joins {
        match *s {
            StatusCode::OK => assert_eq!(v["rtcStatus"], "ISSUED"),
            StatusCode::CONFLICT => assert_eq!(code(v), "CONFERENCE_NOT_LIVE"),
            StatusCode::FORBIDDEN => assert_eq!(code(v), "MEETING_ACCESS_DENIED"),
            other => panic!("unexpected {other}: {v}"),
        }
    }
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(app.scalar(&format!(
        "SELECT count(*) FROM participant_sessions ps JOIN conference_sessions c ON c.id = ps.conference_id
          WHERE c.id='{cid}' AND ps.established_at > c.ending_requested_at AND ps.state IN ('PENDING','CONNECTED')")).await, 0);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')")).await, 0);
    assert!(
        !lk(&app).has_room(&room(cid)),
        "nobody can connect: the room is gone"
    );
    // Every credential handed out is now useless.
    for (_, v) in joins.iter().filter(|(s, _)| *s == StatusCode::OK) {
        assert!(lk(&app)
            .connect(v["rtc"]["credential"].as_str().unwrap())
            .is_err());
    }
}

/// C5. Role change racing removal: removal wins; no grants survive on the provider.
#[tokio::test]
async fn c5_role_change_racing_removal() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "c5", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    for round in 0..4 {
        let p = join_guest(&app, cid, &format!("p{round}")).await;
        connect(&app, cid, &p).await;
        let role = change_role(&app, cid, &owner, p.participant_id, "PRESENTER");
        let remove_uri = format!(
            "/api/v2/conferences/{cid}/participants/{}/remove",
            p.participant_id
        );
        let remove = app.post(&remove_uri, Auth::Bearer(&owner.token), json!({}));
        let ((rs, _), (ms, _)) = tokio::join!(role, remove);
        assert_eq!(ms, StatusCode::OK);
        assert!(matches!(rs, StatusCode::OK | StatusCode::CONFLICT), "{rs}");
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='REMOVED'", p.participant_id)).await, 1);
        assert!(
            lk(&app).participant(&room(cid), &p.identity).is_none(),
            "round {round}: provider participant gone"
        );
    }
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='PROVIDER_COMMAND' AND status='PENDING'")).await, 0);
}

/// C6. Two reconcilers: one holds the lease; repairs are applied once.
#[tokio::test]
async fn c6_two_reconcilers() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c6", "accessPolicy": "PUBLIC" }))
        .await;
    lk(&app).drop_room(&room(cid));
    let r1 = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let r2 = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let (a, b) = tokio::join!(r1.tick(), r2.tick());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(
        [a.skipped_lease, b.skipped_lease]
            .iter()
            .filter(|s| **s)
            .count(),
        1,
        "{a:?} {b:?}"
    );
    assert!(lk(&app).has_room(&room(cid)));
    assert_eq!(
        lk(&app).call_count("CreateRoom"),
        2,
        "initial prepare + exactly one repair"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM rtc_drift_events WHERE conference_id='{cid}' AND kind='LIVE_ROOM_MISSING'")).await, 1);
}

/// C7. Duplicate webhook delivered concurrently is applied exactly once.
#[tokio::test]
async fn c7_duplicate_webhook() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c7", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app).connect(&p.credential).unwrap();
    let body = livekit_mock::webhook_body(
        "participant_joined",
        "EV_c7",
        &room(cid),
        None,
        Some(&p.identity),
    );
    let results = join_all((0..10).map(|_| app.signed_webhook(&body))).await;
    assert_eq!(
        results
            .iter()
            .filter(|(_, v)| v["outcome"] == "APPLIED")
            .count(),
        1,
        "{results:?}"
    );
    assert_eq!(events(&app, cid, "participant.joined").await, 1);
}

struct Counting {
    seen: std::sync::Mutex<Vec<Uuid>>,
    calls: AtomicUsize,
}

#[async_trait]
impl EventPublisher for Counting {
    async fn publish(&self, event: &outbox_events::Model) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(event.id);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Ok(())
    }
}

/// C8. Parallel outbox workers: every realtime row published exactly once; every
/// provider command executed exactly once.
#[tokio::test]
async fn c8_parallel_outbox_workers() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c8", "accessPolicy": "PUBLIC" }))
        .await;
    let mut joined = vec![];
    for i in 0..6 {
        let p = join_guest(&app, cid, &format!("p{i}")).await;
        connect(&app, cid, &p).await;
        joined.push(p);
    }
    // Queue provider commands without inline dispatch (simulate a crashed request path).
    for p in &joined {
        exec(&app, &format!(
            "INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, payload, conference_id, channel)
             VALUES (gen_random_uuid(), 'participant_session', '{s}', 'rtc.participant_remove_requested',
                     '{{\"conferenceId\":\"{cid}\",\"participantSessionId\":\"{s}\",\"reason\":\"TEST\"}}', '{cid}', 'PROVIDER_COMMAND')",
            s = p.session_id
        )).await;
    }
    let realtime_pending = app
        .scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING' AND channel='REALTIME'")
        .await;
    let publisher = Arc::new(Counting {
        seen: Default::default(),
        calls: AtomicUsize::new(0),
    });
    let control = Arc::new(RtcControl::new(app.state.meeting.core.clone()));
    let relays: Vec<_> = (0..4)
        .map(|_| {
            OutboxRelay::with_control(
                app.db.conn.clone(),
                Some(publisher.clone() as Arc<dyn EventPublisher>),
                Some(control.clone()),
            )
        })
        .collect();
    let removes_before = lk(&app).call_count("RemoveParticipant");
    for _ in 0..3 {
        join_all(relays.iter().map(|r| r.tick(5))).await;
    }
    let seen = publisher.seen.lock().unwrap().clone();
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), unique.len(), "no realtime row published twice");
    assert_eq!(seen.len() as i64, realtime_pending);
    assert_eq!(
        lk(&app).call_count("RemoveParticipant") - removes_before,
        joined.len(),
        "each command executed once"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING'")
            .await,
        0
    );
}
