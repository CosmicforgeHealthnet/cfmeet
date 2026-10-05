//! Part L — global concurrency certification (L-C1..L-C12, §52–§58, §66).
//!
//! Requests race for real on a multi-threaded runtime (spawned router calls) against
//! PostgreSQL and the mock LiveKit provider. Each race is repeated where the outcome
//! could depend on scheduling.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use backend::{
    models::outbox_events,
    services::rtc_control::RtcControl,
    workers::outbox_relay::{EventPublisher, OutboxRelay},
};
use chrono::{Duration, Utc};
use common::{artifacts as art, collab::*, rtc::*, *};
use futures_util::future::join_all;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

fn spawn(
    router: &Router,
    method: Method,
    uri: String,
    auth: Option<(&'static str, String)>,
    body: Value,
) -> JoinHandle<(StatusCode, Value)> {
    let router = router.clone();
    tokio::spawn(async move {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some((k, v)) = auth {
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

/// L-C1: parallel start of the same scheduled conference → one LIVE state, one start
/// event, one provider room.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c01_parallel_conference_start() {
    let app = app!(TestApp::livekit());
    for round in 0..3 {
        let owner = app.user("owner").await;
        let space = app
            .create_space(&owner, json!({ "title": format!("c1-{round}") }))
            .await;
        let cohost = app.user("cohost").await;
        app.add_member(
            uuid(&space["id"]),
            &owner,
            &cohost,
            backend::domain::Role::Host,
        )
        .await;
        let (_, c) = app
            .post(
                format!("/api/v2/meeting-spaces/{}/conferences", uuid(&space["id"])),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        let cid = uuid(&c["id"]);
        let uri = format!("/api/v2/conferences/{cid}/start");
        let rooms_before = lk(&app).call_count("CreateRoom");
        let handles = (0..8)
            .map(|i| {
                let who = if i % 2 == 0 { &owner } else { &cohost };
                spawn(
                    &app.router,
                    Method::POST,
                    uri.clone(),
                    bearer(who),
                    json!({}),
                )
            })
            .collect();
        let results = collect(handles).await;
        for (s, v) in &results {
            assert!(
                *s == StatusCode::OK || code(v) == "CONFERENCE_STATE_CONFLICT",
                "{s} {v}"
            );
        }
        assert_eq!(conference_status(&app, cid).await, "LIVE");
        assert_eq!(
            events(&app, cid, "conference.started").await,
            1,
            "one start event"
        );
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='CONFERENCE_START'"
            ))
            .await,
            1
        );
        assert!(lk(&app).has_room(&room(cid)));
        assert!(
            lk(&app).call_count("CreateRoom") - rooms_before <= 8,
            "CreateRoom is idempotent; one room exists"
        );
        assert_eq!(
            lk(&app)
                .room_names()
                .iter()
                .filter(|n| **n == room(cid))
                .count(),
            1
        );
    }
}

/// L-C2: parallel admits of the same waiting participant → one admission, one logical
/// participant, one admission event.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c02_parallel_admit() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "c2", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    for round in 0..3 {
        let (_, w) = app
            .join(
                cid,
                Auth::None,
                json!({ "displayName": format!("w{round}") }),
            )
            .await;
        let pid = uuid(&w["participant"]["id"]);
        let uri = format!("/api/v2/conferences/{cid}/waiting-room/admit");
        let handles = (0..8)
            .map(|_| {
                spawn(
                    &app.router,
                    Method::POST,
                    uri.clone(),
                    bearer(&owner),
                    json!({ "participantIds": [pid] }),
                )
            })
            .collect();
        let results = collect(handles).await;
        let changed = results
            .iter()
            .filter(|(s, v)| {
                *s == StatusCode::OK && v["changed"].to_string().contains(&pid.to_string())
            })
            .count();
        assert_eq!(changed, 1, "exactly one admission: {results:?}");
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM conference_participants WHERE id='{pid}' AND admission_state='ADMITTED'"
            ))
            .await,
            1
        );
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='participant.admitted' AND payload::text LIKE '%{pid}%'"
            ))
            .await,
            1,
            "one admission event"
        );
    }
    let rows = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await;
    let identities = app
        .scalar(&format!(
            "SELECT count(DISTINCT coalesce(user_id::text, guest_identity)) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await;
    assert_eq!(rows, 3, "one logical participant per waiting guest");
    assert_eq!(rows, identities, "no duplicate logical participants");
}

/// L-C3: role change racing removal → removal wins; no provider permission restored;
/// no usable credential afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c03_role_change_vs_remove() {
    let app = app!(TestApp::collab());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "c3", "accessPolicy": "PUBLIC", "defaultRole": "VIEWER" }),
        )
        .await;
    for round in 0..5 {
        let p = join_guest(&app, cid, &format!("p{round}")).await;
        connect(&app, cid, &p).await;
        let role = spawn(
            &app.router,
            Method::PATCH,
            format!(
                "/api/v2/conferences/{cid}/participants/{}/role",
                p.participant_id
            ),
            bearer(&owner),
            json!({ "role": "HOST" }),
        );
        let remove = spawn(
            &app.router,
            Method::POST,
            format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                p.participant_id
            ),
            bearer(&owner),
            json!({}),
        );
        let (rr, mr) = (role.await.unwrap(), remove.await.unwrap());
        assert_eq!(mr.0, StatusCode::OK, "{}", mr.1);
        assert!(
            matches!(
                rr.0,
                StatusCode::OK | StatusCode::CONFLICT | StatusCode::NOT_FOUND
            ),
            "{rr:?}"
        );
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='REMOVED'",
                p.participant_id
            ))
            .await,
            1,
            "terminal removal wins"
        );
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{}' AND state IN ('PENDING','CONNECTED')",
                p.participant_id
            ))
            .await,
            0
        );
        assert!(lk(&app).participant(&room(cid), &p.identity).is_none());
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/media-credential"),
                p.auth(),
                json!({ "participantSessionId": p.session_id }),
            )
            .await;
        assert!(s.is_client_error(), "no credential after removal: {s} {v}");
        assert!(
            !matches!(code(&v), "VALIDATION_ERROR" | "FEATURE_DISABLED"),
            "{v}"
        );
        // A stale credential cannot reconnect with restored grants.
        if lk(&app).connect(&p.credential).is_ok() {
            let (_, v) = provider_event(&app, "participant_joined", cid, Some(&p.identity)).await;
            assert_ne!(v["outcome"], "APPLIED", "stale identity is evicted: {v}");
            assert!(lk(&app).participant(&room(cid), &p.identity).is_none());
        }
    }
}

/// L-C4: END racing joins (repeated): once ENDING wins no usable media credential is
/// issued; joins that won earlier are terminated with the conference.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c04_end_vs_join_fails_closed() {
    let app = app!(TestApp::livekit());
    for round in 0..4 {
        let owner = app.user("owner").await;
        let (_, cid) = app
            .live_conference(
                &owner,
                json!({ "title": format!("c4-{round}"), "accessPolicy": "PUBLIC" }),
            )
            .await;
        let mut handles: Vec<_> = (0..10)
            .map(|i| {
                spawn(
                    &app.router,
                    Method::POST,
                    format!("/api/v2/conferences/{cid}/join"),
                    None,
                    json!({ "displayName": format!("g{i}") }),
                )
            })
            .collect();
        handles.insert(
            round % 5,
            spawn(
                &app.router,
                Method::POST,
                format!("/api/v2/conferences/{cid}/end"),
                bearer(&owner),
                json!({}),
            ),
        );
        let results = collect(handles).await;
        for (s, v) in &results {
            assert!(
                *s == StatusCode::OK || code(v) == "CONFERENCE_NOT_LIVE",
                "{s} {v}"
            );
        }
        assert_eq!(conference_status(&app, cid).await, "ENDED");
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')"
            ))
            .await,
            0,
            "every session that won the race was terminated"
        );
        for (_, v) in results
            .iter()
            .filter(|(s, v)| *s == StatusCode::OK && v["rtc"].is_object())
        {
            assert!(
                lk(&app)
                    .connect(v["rtc"]["credential"].as_str().unwrap())
                    .is_err(),
                "credential unusable after END"
            );
        }
        let (s, v) = app
            .join(cid, Auth::None, json!({ "displayName": "late" }))
            .await;
        assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    }
}

/// L-C5: several scheduler instances materializing the same recurrence produce each
/// occurrence once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c05_parallel_schedulers_same_occurrence() {
    let app = app!(TestApp::new());
    let owner = app.user("owner").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "c5", "timezone": "America/New_York" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    // Starts beyond the materialization horizon: the PUT creates nothing, so the racing
    // scheduler instances all see the same due work.
    let start = (Utc::now() + Duration::days(45))
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": "FREQ=DAILY;COUNT=5", "startLocal": start, "durationMinutes": 30 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await,
        0
    );
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let meeting = app.state.meeting.clone();
            tokio::spawn(async move {
                backend::workers::ConferenceScheduler::new(meeting)
                    .tick(Utc::now() + Duration::days(25))
                    .await
                    .unwrap()
                    .materialized
            })
        })
        .collect();
    let mut created = 0;
    for h in handles {
        created += h.await.unwrap();
    }
    let total = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await;
    let distinct = app
        .scalar(&format!(
            "SELECT count(DISTINCT occurrence_key) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await;
    assert_eq!(total, 5, "COUNT=5 series materialized exactly once");
    assert_eq!(total, distinct);
    assert_eq!(
        created as i64, total,
        "instances report only their own inserts"
    );
}

async fn legacy_due_meetings(app: &TestApp, host: Uuid, status: &str, n: usize) -> Vec<Uuid> {
    let mut ids = vec![];
    for _ in 0..n {
        let id = Uuid::new_v4();
        let code = format!("A{}", &id.simple().to_string()[..7])
            .to_uppercase()
            .replace(['0', '1', 'O', 'I'], "Q");
        app.db
            .conn
            .execute_unprepared(&format!(
                "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
                 VALUES ('{id}', '{code}', '{host}', '{host}', 'auto', false, now() - interval '2 hours', now() - interval '1 hour', '{status}')"
            ))
            .await
            .unwrap();
        ids.push(id);
    }
    ids
}

/// L-C6: concurrent auto-start workers. v2 has no auto-start (a conference is started
/// by an authorized host, raced in L-C1); the legacy `MeetingAutoStartWorker` instances
/// start each due meeting — and log it — exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c06_two_auto_start_workers() {
    let app = app!(TestApp::new());
    let host = app.user("host").await;
    let ids = legacy_due_meetings(&app, host.id, "scheduled", 6).await;
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let db = app.db.conn.clone();
            tokio::spawn(async move {
                backend::workers::MeetingAutoStartWorker::new(db)
                    .process_batch()
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut started = 0;
    for h in handles {
        started += h.await.unwrap();
    }
    assert_eq!(
        started, 6,
        "each due meeting started by exactly one instance"
    );
    for id in ids {
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM session_logs WHERE meeting_id='{id}' AND event_type='meeting_start'"
            ))
            .await,
            1,
            "one start log"
        );
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM meetings WHERE id='{id}' AND status='ongoing'"
            ))
            .await,
            1
        );
    }
}

/// L-C7: concurrent auto-end workers — v2 scheduler instances request END once per
/// conference (one termination command); legacy auto-end instances end each meeting once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c07_two_auto_end_workers() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let mut cids = vec![];
    for i in 0..3 {
        let (_, cid) = app
            .live_conference(
                &owner,
                json!({ "title": format!("c7-{i}"), "accessPolicy": "PUBLIC" }),
            )
            .await;
        app.db
            .conn
            .execute_unprepared(&format!(
                "UPDATE conference_sessions SET scheduled_start_at = now() - interval '3 hours', scheduled_end_at = now() - interval '2 hours' WHERE id='{cid}'"
            ))
            .await
            .unwrap();
        cids.push(cid);
    }
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let meeting = app.state.meeting.clone();
            tokio::spawn(async move {
                backend::workers::ConferenceScheduler::new(meeting)
                    .tick(Utc::now())
                    .await
                    .unwrap()
                    .end_requested
            })
        })
        .collect();
    let mut ended = 0;
    for h in handles {
        ended += h.await.unwrap();
    }
    assert_eq!(ended, 3, "each conference ended by exactly one scheduler");
    for cid in cids {
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.room_terminate_requested'"
            ))
            .await,
            1
        );
        assert_eq!(events(&app, cid, "conference.ending").await.max(1), 1);
        assert!(matches!(
            conference_status(&app, cid).await.as_str(),
            "ENDING" | "ENDED"
        ));
    }
    // Legacy auto-end instances.
    let host = app.user("host").await;
    let ids = legacy_due_meetings(&app, host.id, "ongoing", 5).await;
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let db = app.db.conn.clone();
            tokio::spawn(async move {
                backend::workers::MeetingAutoEndWorker::new(db)
                    .process_batch()
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut ended = 0;
    for h in handles {
        ended += h.await.unwrap();
    }
    assert_eq!(ended, 5);
    for id in ids {
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM session_logs WHERE meeting_id='{id}' AND event_type='meeting_end'"
            ))
            .await,
            1,
            "one end log"
        );
    }
}

/// L-C8: concurrent recording starts → one active recording, one provider capture.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c08_parallel_recording_start() {
    let app = app!(TestApp::artifacts());
    let (owner, sid, cid, _) = art::meeting(&app, 0).await;
    let host = app.user("host-b").await;
    app.add_member(sid, &owner, &host, backend::domain::Role::Host)
        .await;
    let handles = (0..6)
        .map(|i| {
            spawn(
                &app.router,
                Method::POST,
                format!("/api/v2/conferences/{cid}/recordings"),
                bearer(if i % 2 == 0 { &owner } else { &host }),
                json!({}),
            )
        })
        .collect();
    let results = collect(handles).await;
    let accepted = results
        .iter()
        .filter(|(s, _)| *s == StatusCode::ACCEPTED)
        .count();
    assert_eq!(accepted, 1, "{results:?}");
    for (s, v) in results.iter().filter(|(s, _)| *s != StatusCode::ACCEPTED) {
        assert_eq!(
            (*s, code(v)),
            (StatusCode::CONFLICT, "RECORDING_ALREADY_ACTIVE")
        );
    }
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND artifact_type='RECORDING' AND status NOT IN ('FAILED','DELETED')"
        ))
        .await,
        1
    );
}

/// L-C9: two hosts moving the same participant into different breakouts at once never
/// leave it active in two rooms (one assignment, one media context).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c09_breakout_move_race() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let cohost = app.user("cohost").await;
    let cj = join_user(&app, cid, &cohost).await;
    change_role(&app, cid, &owner, cj.participant_id, "CO_HOST").await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    for &r in &rooms {
        open_room(&app, cid, &owner, r).await;
    }
    for round in 0..6 {
        let uri = format!("{}/breakouts/move", base(cid));
        let a = spawn(
            &app.router,
            Method::POST,
            uri.clone(),
            bearer(&owner),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": rooms[round % 2] }),
        );
        let b = spawn(
            &app.router,
            Method::POST,
            uri,
            bearer(&cohost),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": rooms[(round + 1) % 2] }),
        );
        let (ra, rb) = (a.await.unwrap(), b.await.unwrap());
        for r in [&ra, &rb] {
            assert!(
                r.0 == StatusCode::OK || code(&r.1) == "BREAKOUT_ASSIGNMENT_CONFLICT",
                "{r:?}"
            );
        }
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM breakout_assignments WHERE participant_id='{}' AND unassigned_at IS NULL",
                g[0].participant_id
            ))
            .await,
            1,
            "exactly one active assignment"
        );
        let (s, st) = app
            .post(
                format!("{}/media-credential", base(cid)),
                g[0].auth(),
                json!({ "participantSessionId": g[0].session_id }),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{st}");
        let target = st["mediaContext"]["breakoutId"].clone();
        assert!(
            rooms.iter().any(|r| json!(r) == target),
            "credential targets exactly one breakout: {st}"
        );
    }
}

/// L-C10: two reconcilers → one holds the lease; the repair is applied once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c10_two_reconcilers() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c10", "accessPolicy": "PUBLIC" }))
        .await;
    lk(&app).drop_room(&room(cid));
    let creates_before = lk(&app).call_count("CreateRoom");
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let core = app.state.meeting.core.clone();
            tokio::spawn(async move {
                backend::workers::ConferenceReconciler::new(core)
                    .tick()
                    .await
                    .unwrap()
            })
        })
        .collect();
    let reports: Vec<_> = join_all(handles)
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let workers_ran = reports.iter().filter(|r| !r.skipped_lease).count();
    assert!(workers_ran >= 1);
    assert!(lk(&app).has_room(&room(cid)));
    assert_eq!(
        lk(&app).call_count("CreateRoom") - creates_before,
        1,
        "one repair"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM rtc_drift_events WHERE conference_id='{cid}' AND kind='LIVE_ROOM_MISSING'"
        ))
        .await,
        1
    );
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
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        Ok(())
    }
}

/// L-C11: parallel outbox relays publish every realtime row exactly once and execute
/// every provider command exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c11_two_outbox_workers() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "c11", "accessPolicy": "PUBLIC" }))
        .await;
    let mut sessions = vec![];
    for i in 0..5 {
        let p = join_guest(&app, cid, &format!("p{i}")).await;
        connect(&app, cid, &p).await;
        sessions.push(p.session_id);
    }
    for s in &sessions {
        exec(&app, &format!(
            "INSERT INTO outbox_events (id, aggregate_type, aggregate_id, event_type, payload, conference_id, channel)
             VALUES (gen_random_uuid(), 'participant_session', '{s}', 'rtc.participant_remove_requested',
                     '{{\"conferenceId\":\"{cid}\",\"participantSessionId\":\"{s}\",\"reason\":\"TEST\"}}', '{cid}', 'PROVIDER_COMMAND')"
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
    let removes_before = lk(&app).call_count("RemoveParticipant");
    for _ in 0..4 {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let relay = OutboxRelay::with_control(
                    app.db.conn.clone(),
                    Some(publisher.clone() as Arc<dyn EventPublisher>),
                    Some(control.clone()),
                );
                tokio::spawn(async move { relay.tick(3).await.unwrap() })
            })
            .collect();
        join_all(handles).await;
    }
    let seen = publisher.seen.lock().unwrap().clone();
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), unique.len(), "no realtime row published twice");
    assert_eq!(seen.len() as i64, realtime_pending);
    assert_eq!(
        lk(&app).call_count("RemoveParticipant") - removes_before,
        sessions.len(),
        "each provider command executed once"
    );
}

/// L-C12: duplicated artefact retention workers delete a due artefact once.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn l_c12_artifact_retention_worker_duplication() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, _) = art::meeting(&app, 0).await;
    let (rid, _) = art::recording_ready(&app, cid, &owner).await;
    let key = art::storage_key(&app, rid).await;
    exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 minute' WHERE id='{rid}'")).await;
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let core = app.state.meeting.core.clone();
            tokio::spawn(async move {
                backend::services::artifacts::retention::ArtifactRetentionWorker::new(core)
                    .tick()
                    .await
                    .unwrap()
                    .deleted
            })
        })
        .collect();
    let mut deleted = 0;
    for h in handles {
        deleted += h.await.unwrap();
    }
    assert_eq!(deleted, 1);
    assert_eq!(art::audits(&app, cid, "ARTIFACT_DELETED").await, 1);
    assert_eq!(art::artifact_status(&app, rid).await, "DELETED");
    assert!(!app.storage.contains(&key));
}
