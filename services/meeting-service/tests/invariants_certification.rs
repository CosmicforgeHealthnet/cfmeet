//! Part M §24 / Part N §91 — permanent invariant and security-matrix certification.
//!
//! Most invariants are already certified by the A–L suites (see
//! `docs/phase2-certification/02-requirements-matrix.md` for the mapping); this
//! file adds the cases that had no dedicated regression:
//! - ADMITTED / DENIED exclusivity under a real admit-vs-deny race;
//! - one RTC room ↔ one ConferenceSession;
//! - READY recording ↔ conference + validated storage object;
//! - M-S4: every moderation action of host A against a participant of conference B
//!   fails before any provider call and leaves B unchanged.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use common::{artifacts as art, collab::*, rtc::*, *};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
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

fn spawn_post(
    router: &Router,
    uri: String,
    token: String,
    body: Value,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let router = router.clone();
    tokio::spawn(async move {
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
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

/// §91 — a participant can never be both ADMITTED and DENIED: admit and deny race
/// for the same waiting participant; exactly one decision wins and is the only one
/// audited and published.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn inv_admitted_and_denied_are_exclusive() {
    let app = app!(TestApp::new());
    let owner = app.user("owner").await;
    for round in 0..5 {
        let (_, cid) = app
            .live_conference(&owner, json!({ "title": format!("race {round}"), "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }))
            .await;
        let (_, g) = app
            .join(cid, Auth::None, json!({ "displayName": "waiting" }))
            .await;
        assert_eq!(g["participant"]["admissionState"], "WAITING", "{g}");
        let pid = uuid(&g["participant"]["id"]);
        let mut handles = vec![];
        for i in 0..10 {
            let action = if i % 2 == 0 { "admit" } else { "deny" };
            handles.push(spawn_post(
                &app.router,
                format!("/api/v2/conferences/{cid}/waiting-room/{action}"),
                owner.token.clone(),
                json!({ "participantIds": [pid] }),
            ));
        }
        let mut changed = 0;
        for h in handles {
            let (s, v) = h.await.unwrap();
            assert!(s.is_success() || s == StatusCode::CONFLICT, "{s} {v}");
            changed += v["changed"].as_array().map(|a| a.len()).unwrap_or(0);
        }
        assert_eq!(changed, 1, "exactly one decision applied (round {round})");
        let state = app
            .db
            .conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT admission_state FROM conference_participants WHERE id='{pid}'"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<String>(0)
            .unwrap();
        assert!(state == "ADMITTED" || state == "DENIED", "{state}");
        let decisions = app
            .scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action IN ('ADMIT','DENY')"))
            .await;
        assert_eq!(decisions, 1, "one audited decision");
        let published = app
            .scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type IN ('participant.admitted','participant.denied')"))
            .await;
        assert_eq!(published, 1, "one published decision");
        if state == "DENIED" {
            let sessions = app
                .scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{pid}' AND state IN ('PENDING','CONNECTED')"))
                .await;
            assert_eq!(sessions, 0, "a denied participant holds no live session");
        }
    }
}

/// §91 — an RTC room maps to exactly one ConferenceSession: room names are unique
/// per conference (database constraint) and the provider callback for one room can
/// never resolve to another conference.
#[tokio::test]
async fn inv_rtc_room_maps_to_exactly_one_conference() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, a) = app
        .live_conference(&owner, json!({ "title": "A", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, b) = app
        .live_conference(&owner, json!({ "title": "B", "accessPolicy": "PUBLIC" }))
        .await;
    let ga = join_guest(&app, a, "ga").await;
    connect(&app, a, &ga).await;
    let gb = join_guest(&app, b, "gb").await;
    connect(&app, b, &gb).await;
    let name = |cid: Uuid| {
        format!("SELECT count(DISTINCT rtc_room_name) FROM conference_sessions WHERE id='{cid}' AND rtc_room_name IS NOT NULL")
    };
    assert_eq!(app.scalar(&name(a)).await, 1);
    assert_eq!(app.scalar(&name(b)).await, 1);
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_sessions c1 JOIN conference_sessions c2 ON c1.rtc_room_name = c2.rtc_room_name AND c1.id <> c2.id WHERE c1.id IN ('{a}','{b}')")).await,
        0
    );
    // The database refuses a second conference claiming the same room.
    let dup = app
        .db
        .conn
        .execute_unprepared(&format!(
            "UPDATE conference_sessions SET rtc_room_name = (SELECT rtc_room_name FROM conference_sessions WHERE id='{a}') WHERE id='{b}'"
        ))
        .await;
    assert!(dup.is_err(), "duplicate room mapping must be rejected");
    // A provider join for B's identity reported in A's room never touches B.
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &room(a),
        None,
        Some(&gb.identity),
    );
    let (s, v) = app.signed_webhook(&body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_ne!(v["outcome"], "CONNECTED", "{v}");
    assert_eq!(session_state(&app, gb.session_id).await, "CONNECTED");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE id='{}' AND conference_id='{b}'",
            gb.session_id
        ))
        .await,
        1
    );
}

/// §91 — a READY recording always references its conference and a validated,
/// existing storage object; READY without a validated object is impossible.
#[tokio::test]
async fn inv_ready_recording_references_conference_and_storage() {
    let app = app!(TestApp::artifacts());
    let (owner, _sid, cid, _g) = art::meeting(&app, 2).await;
    let (rid, bytes) = art::recording_ready(&app, cid, &owner).await;
    let key = art::storage_key(&app, rid).await;
    assert!(!key.is_empty());
    assert!(app.storage.contains(&key), "object exists in storage");
    assert_eq!(app.storage.get_raw(&key).unwrap().len(), bytes.len());
    assert!(!key.starts_with("http"), "opaque key, never a URL");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts a JOIN conference_sessions c ON c.id = a.conference_id \
             WHERE a.id='{rid}' AND a.conference_id='{cid}' AND a.status='READY' AND a.checksum_sha256 IS NOT NULL \
             AND a.size_bytes = {} AND a.finalized_at IS NOT NULL",
            bytes.len()
        ))
        .await,
        1
    );
    // The database refuses READY without a validated object.
    let (_, cid2) = app
        .live_conference(
            &owner,
            json!({ "title": "second", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let forged = app
        .db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meeting_artifacts (id, conference_id, artifact_type, status) VALUES (gen_random_uuid(), '{cid2}', 'RECORDING', 'READY')"
        ))
        .await;
    assert!(
        forged.is_err(),
        "READY without storage object must be rejected"
    );
    // Moving an artefact to a different conference is impossible for its children
    // (id, conference_id) and the record stays scoped to its own conference.
    let (s, _) = app
        .get(
            format!("/api/v2/conferences/{cid2}/recordings/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "recording is scoped to its conference"
    );
}

/// M-S4 — host of conference A cannot admit, deny, remove, ban, change the role of,
/// mute, lock or breakout-move a participant of conference B: every attempt fails
/// before any provider call, through A's path and through B's path.
#[tokio::test]
async fn m_s04_host_a_cannot_moderate_conference_b() {
    let app = app!(TestApp::collab());
    let host_a = app.user("host-a").await;
    let host_b = app.user("host-b").await;
    let (_, a) = app
        .live_conference(&host_a, json!({ "title": "A", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, b) = app
        .live_conference(
            &host_b,
            json!({ "title": "B", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        )
        .await;
    let a_room = create_rooms(&app, a, &host_a, json!({ "count": 1 })).await[0];
    open_room(&app, a, &host_a, a_room).await;
    // B: one connected member and one waiting guest.
    let member = app.user("b-member").await;
    let victim = join_user(&app, b, &member).await;
    connect(&app, b, &victim).await;
    let (_, w) = app
        .join(b, Auth::None, json!({ "displayName": "b-waiting" }))
        .await;
    assert_eq!(w["participant"]["admissionState"], "WAITING", "{w}");
    let waiting = uuid(&w["participant"]["id"]);
    let vp = victim.participant_id;

    let fingerprint = |app: &TestApp| {
        let sql = format!(
            "SELECT md5(string_agg(t, '|' ORDER BY t)) FROM (\
               SELECT concat_ws(',', id, role, admission_state, removed_at, banned_at, audio_locked_at, video_locked_at, screen_share_locked_at, breakout_visit_room_id, version) AS t FROM conference_participants WHERE conference_id='{b}' \
               UNION ALL SELECT concat_ws(',', id, state) FROM participant_sessions WHERE conference_id='{b}' \
               UNION ALL SELECT concat_ws(',', id, breakout_room_id) FROM breakout_assignments WHERE conference_id='{b}' \
               UNION ALL SELECT concat_ws(',', id) FROM conference_bans WHERE conference_id='{b}' OR meeting_space_id = (SELECT meeting_space_id FROM conference_sessions WHERE id='{b}')) x"
        );
        let conn = app.db.conn.clone();
        async move {
            conn.query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql,
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<Option<String>>(0)
            .unwrap()
        }
    };
    let provider_touches = |app: &TestApp| {
        [
            "RemoveParticipant",
            "UpdateParticipant",
            "MutePublishedTrack",
        ]
        .iter()
        .map(|m| {
            lk(app)
                .calls(m)
                .into_iter()
                .filter(|(_, i)| *i == victim.identity)
                .count()
        })
        .sum::<usize>()
    };
    let before = fingerprint(&app).await;
    let touches = provider_touches(&app);

    let actions: Vec<(&str, Method, String, Value)> = vec![
        (
            "admit",
            Method::POST,
            "waiting-room/admit".into(),
            json!({ "participantIds": [waiting] }),
        ),
        (
            "deny",
            Method::POST,
            "waiting-room/deny".into(),
            json!({ "participantIds": [waiting] }),
        ),
        (
            "remove",
            Method::POST,
            format!("participants/{vp}/remove"),
            json!({}),
        ),
        (
            "ban",
            Method::POST,
            format!("participants/{vp}/ban"),
            json!({}),
        ),
        (
            "role",
            Method::PATCH,
            format!("participants/{vp}/role"),
            json!({ "role": "CO_HOST" }),
        ),
        (
            "mute",
            Method::POST,
            format!("participants/{vp}/mute"),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            "audio-lock",
            Method::POST,
            format!("participants/{vp}/audio-lock"),
            json!({}),
        ),
        (
            "video-lock",
            Method::POST,
            format!("participants/{vp}/video-lock"),
            json!({}),
        ),
        (
            "screen-share-lock",
            Method::POST,
            format!("participants/{vp}/screen-share-lock"),
            json!({}),
        ),
        (
            "breakout-move",
            Method::POST,
            "breakouts/move".into(),
            json!({ "participantId": vp, "toBreakoutId": a_room }),
        ),
    ];
    for (name, method, path, body) in actions {
        for conf in [a, b] {
            let uri = format!("/api/v2/conferences/{conf}/{path}");
            let (s, v) = app
                .call(
                    method.clone(),
                    &uri,
                    Auth::Bearer(&host_a.token),
                    Some(body.clone()),
                )
                .await;
            assert!(
                matches!(
                    s,
                    StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::CONFLICT
                ) || (s == StatusCode::OK
                    && v["changed"]
                        .as_array()
                        .map(|c| c.is_empty())
                        .unwrap_or(false)),
                "{name} via {conf}: {s} {v}"
            );
        }
    }
    assert_eq!(fingerprint(&app).await, before, "conference B unchanged");
    assert_eq!(
        provider_touches(&app),
        touches,
        "no provider call against B's participant"
    );
    assert!(
        lk(&app).participant(&room(b), &victim.identity).is_some(),
        "still connected at the provider"
    );
    assert_eq!(
        audits(&app, b, "BAN").await + audits(&app, b, "REMOVE").await,
        0
    );
}
