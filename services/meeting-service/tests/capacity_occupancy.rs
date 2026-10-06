//! Interactive capacity = CURRENT logical occupancy (Part M load-certification fix).
//!
//! A logical participant occupies one interactive seat while it is ADMITTED and has at
//! least one live ParticipantSession (PENDING / CONNECTED, not past `expires_at`). Several
//! devices count once; the seat is released when the last live session ends. ADMITTED
//! is the persistent admission authorization (rejoin without waiting again), not a seat.
//! The dedicated viewer pool (`viewerCapacity`) keeps its admitted-audience count.

mod common;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use common::*;
use sea_orm::ConnectionTrait;
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

/// Mirror of the occupancy rule (interactive pool, no dedicated viewer pool).
async fn occupancy(app: &TestApp, cid: Uuid) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM conference_participants p WHERE p.conference_id = '{cid}' \
           AND p.admission_state = 'ADMITTED' AND EXISTS (SELECT 1 FROM participant_sessions s \
           WHERE s.conference_participant_id = p.id AND s.state IN ('PENDING','CONNECTED') \
             AND (s.expires_at IS NULL OR s.expires_at > now()))"
    ))
    .await
}

struct Guest {
    pid: Uuid,
    token: String,
    session: Option<Uuid>,
}

/// Guest join that must be admitted immediately (open room) and get a session.
async fn guest_in(app: &TestApp, cid: Uuid, name: &str) -> Guest {
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": name }))
        .await;
    assert_eq!(s, StatusCode::OK, "{name}: {v}");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED", "{v}");
    assert_eq!(v["rtcStatus"], "ISSUED", "{v}");
    Guest {
        pid: uuid(&v["participant"]["id"]),
        token: v["participantToken"].as_str().unwrap().to_string(),
        session: Some(uuid(&v["participantSession"]["id"])),
    }
}

/// Guest join into a waiting room (WAITING, no session).
async fn guest_waiting(app: &TestApp, cid: Uuid, name: &str) -> Guest {
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": name }))
        .await;
    assert_eq!(s, StatusCode::OK, "{name}: {v}");
    assert_eq!(v["participant"]["admissionState"], "WAITING", "{v}");
    Guest {
        pid: uuid(&v["participant"]["id"]),
        token: v["participantToken"].as_str().unwrap().to_string(),
        session: None,
    }
}

async fn rejoin(app: &TestApp, cid: Uuid, g: &Guest) -> (StatusCode, Value) {
    app.join(cid, Auth::Participant(&g.token), json!({})).await
}

async fn leave(app: &TestApp, cid: Uuid, auth: Auth<'_>, session: Option<Uuid>) {
    let body = match session {
        Some(s) => json!({ "participantSessionId": s }),
        None => json!({}),
    };
    let (s, v) = app
        .post(&format!("/api/v2/conferences/{cid}/leave"), auth, body)
        .await;
    assert!(s.is_success(), "leave: {s} {v}");
}

async fn admit(app: &TestApp, cid: Uuid, host: &User, pid: Uuid) -> (StatusCode, Value) {
    app.post(
        &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
        Auth::Bearer(&host.token),
        json!({ "participantIds": [pid] }),
    )
    .await
}

fn open_room(capacity: i32) -> Value {
    json!({ "title": "capacity", "accessPolicy": "PUBLIC", "interactiveCapacity": capacity })
}

fn waiting_room(capacity: i32) -> Value {
    json!({ "title": "capacity", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE",
            "interactiveCapacity": capacity })
}

/// A + H — an admitted participant with a live session holds one seat; at true
/// simultaneous capacity the next interactive admission fails (join and host admit).
#[tokio::test]
async fn cap_a_h_live_session_holds_seat_and_full_room_refuses() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, open_room(2)).await;
    guest_in(&app, cid, "a").await;
    assert_eq!(occupancy(&app, cid).await, 1);
    guest_in(&app, cid, "b").await;
    assert_eq!(occupancy(&app, cid).await, 2);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "c" }))
        .await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{s} {v}");
    assert_eq!(occupancy(&app, cid).await, 2, "refusal took no seat");

    // Host admission path at capacity.
    let (_, wcid) = app.live_conference(&owner, waiting_room(1)).await;
    let first = guest_waiting(&app, wcid, "w1").await;
    let (s, v) = admit(&app, wcid, &owner, first.pid).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = rejoin(&app, wcid, &first).await;
    assert_eq!(v["rtcStatus"], "ISSUED", "{s} {v}");
    let second = guest_waiting(&app, wcid, "w2").await;
    let (_, v) = admit(&app, wcid, &owner, second.pid).await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='WAITING'", second.pid)).await, 1);
}

/// B + C + D + E — two devices count once; one device leaving keeps the seat; the last
/// device leaving releases it; the participant stays ADMITTED after leaving.
#[tokio::test]
async fn cap_b_c_d_e_multi_device_counts_once_last_session_releases() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("multi").await;
    let (_, cid) = app.live_conference(&owner, open_room(2)).await;
    let (_, laptop) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    let (_, phone) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    let pid = uuid(&laptop["participant"]["id"]);
    assert_eq!(
        uuid(&phone["participant"]["id"]),
        pid,
        "one logical participant"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{pid}' AND state IN ('PENDING','CONNECTED')")).await, 2);
    // B: two devices → one seat; one more participant fits, the next does not.
    assert_eq!(occupancy(&app, cid).await, 1);
    guest_in(&app, cid, "g1").await;
    assert_eq!(occupancy(&app, cid).await, 2);
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g2" }))
        .await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
    // C: the laptop leaves, the phone stays → the seat is still held.
    leave(
        &app,
        cid,
        Auth::Bearer(&user.token),
        Some(uuid(&laptop["participantSession"]["id"])),
    )
    .await;
    assert_eq!(occupancy(&app, cid).await, 2);
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "g2" }))
        .await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
    // D: the last device leaves → the seat is released.
    leave(
        &app,
        cid,
        Auth::Bearer(&user.token),
        Some(uuid(&phone["participantSession"]["id"])),
    )
    .await;
    assert_eq!(occupancy(&app, cid).await, 1);
    guest_in(&app, cid, "g2").await;
    assert_eq!(occupancy(&app, cid).await, 2);
    // E: admission authorization persists after leaving.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND admission_state='ADMITTED'")).await, 1);
}

/// F — a previously admitted participant reconnects without waiting again when a seat is
/// free, is refused while the room is genuinely full, and a reconnect that supersedes its
/// own live session never needs a second seat (even in a full room).
#[tokio::test]
async fn cap_f_reconnect_follows_occupancy() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, waiting_room(2)).await;
    let mut gs = vec![];
    for n in ["a", "b", "c"] {
        let g = guest_waiting(&app, cid, n).await;
        gs.push(g);
    }
    // a and b admitted and connected; a leaves; c admitted and connected.
    for g in &mut gs[..2] {
        assert_eq!(admit(&app, cid, &owner, g.pid).await.0, StatusCode::OK);
        let (_, v) = rejoin(&app, cid, g).await;
        assert_eq!(v["rtcStatus"], "ISSUED", "{v}");
        g.session = Some(uuid(&v["participantSession"]["id"]));
    }
    leave(&app, cid, Auth::Participant(&gs[0].token), None).await;
    assert_eq!(admit(&app, cid, &owner, gs[2].pid).await.0, StatusCode::OK);
    let (_, v) = rejoin(&app, cid, &gs[2]).await;
    assert_eq!(v["rtcStatus"], "ISSUED", "{v}");
    assert_eq!(occupancy(&app, cid).await, 2);
    // Full room: a (ADMITTED, no live session) cannot take a third seat.
    let (_, v) = rejoin(&app, cid, &gs[0]).await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
    // b reconnects (supersedes its own session) in the full room: still its one seat.
    let (s, v) = app
        .join(
            cid,
            Auth::Participant(&gs[1].token),
            json!({ "previousSessionId": gs[1].session }),
        )
        .await;
    assert_eq!(v["rtcStatus"], "ISSUED", "{s} {v}");
    assert_eq!(occupancy(&app, cid).await, 2);
    // A seat frees up → a reconnects directly (no waiting room again).
    leave(&app, cid, Auth::Participant(&gs[2].token), None).await;
    let (_, v) = rejoin(&app, cid, &gs[0]).await;
    assert_eq!(v["participant"]["admissionState"], "ADMITTED", "{v}");
    assert_eq!(v["rtcStatus"], "ISSUED", "{v}");
    assert_eq!(occupancy(&app, cid).await, 2);
}

/// G — 1 000 sequential distinct join → admit → connect → leave cycles through a room
/// of capacity 3 with 2 permanent occupants (every cycle fills the room exactly) never
/// exhaust it: departed participants release their seats.
#[tokio::test]
async fn cap_g_thousand_distinct_cycles_never_exhaust_room() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, waiting_room(3)).await;
    for n in ["resident-1", "resident-2"] {
        let g = guest_waiting(&app, cid, n).await;
        assert_eq!(admit(&app, cid, &owner, g.pid).await.0, StatusCode::OK);
        assert_eq!(rejoin(&app, cid, &g).await.1["rtcStatus"], "ISSUED");
    }
    for i in 0..1000 {
        let g = guest_waiting(&app, cid, &format!("cycle-{i}")).await;
        let (s, v) = admit(&app, cid, &owner, g.pid).await;
        assert_eq!(s, StatusCode::OK, "cycle {i} admit: {v}");
        let (s, v) = rejoin(&app, cid, &g).await;
        assert_eq!(v["rtcStatus"], "ISSUED", "cycle {i} connect: {s} {v}");
        assert_eq!(
            occupancy(&app, cid).await,
            3,
            "cycle {i}: room exactly full"
        );
        leave(&app, cid, Auth::Participant(&g.token), None).await;
    }
    assert_eq!(occupancy(&app, cid).await, 2);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admission_state='ADMITTED'")).await, 1002, "admission history kept");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admission_state='WAITING'")).await, 0);
}

fn spawn_join(router: &Router, cid: Uuid, token: Option<String>, body: Value) -> JoinHandle<Value> {
    let router = router.clone();
    tokio::spawn(async move {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/v2/conferences/{cid}/join"))
            .header("content-type", "application/json");
        if let Some(t) = token {
            req = req.header("x-participant-token", t);
        }
        let resp = router
            .oneshot(req.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    })
}

/// I — concurrent requests competing for the final seat cannot overbook it (new
/// admissions, and previously admitted participants reconnecting).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn cap_i_concurrent_last_seat_never_overbooked() {
    let app = app!();
    let owner = app.user("owner").await;
    for round in 0..3 {
        let (_, cid) = app.live_conference(&owner, open_room(3)).await;
        guest_in(&app, cid, "r1").await;
        guest_in(&app, cid, "r2").await;
        let hs: Vec<_> = (0..20)
            .map(|i| {
                spawn_join(
                    &app.router,
                    cid,
                    None,
                    json!({ "displayName": format!("race-{round}-{i}") }),
                )
            })
            .collect();
        let mut won = 0;
        for h in hs {
            let v = h.await.unwrap();
            if v["rtcStatus"] == "ISSUED" {
                won += 1;
            } else {
                assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
            }
        }
        assert_eq!(won, 1, "exactly one new participant takes the last seat");
        assert_eq!(occupancy(&app, cid).await, 3);
        app.post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    }
    // Reconnect race: 10 admitted-but-departed participants, one free seat.
    let (_, cid) = app.live_conference(&owner, open_room(2)).await;
    let mut departed = vec![];
    for i in 0..10 {
        let g = guest_in(&app, cid, &format!("d{i}")).await;
        leave(&app, cid, Auth::Participant(&g.token), None).await;
        departed.push(g);
    }
    guest_in(&app, cid, "resident").await;
    let hs: Vec<_> = departed
        .iter()
        .map(|g| spawn_join(&app.router, cid, Some(g.token.clone()), json!({})))
        .collect();
    let mut won = 0;
    for h in hs {
        let v = h.await.unwrap();
        if v["rtcStatus"] == "ISSUED" {
            won += 1;
        } else {
            assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
        }
    }
    assert_eq!(
        won, 1,
        "exactly one reconnecting participant takes the last seat"
    );
    assert_eq!(occupancy(&app, cid).await, 2);
}

/// J — removal, ban, session expiry and conference end release / terminate occupancy.
#[tokio::test]
async fn cap_j_remove_ban_expiry_end_release_occupancy() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, open_room(1)).await;
    let base = format!("/api/v2/conferences/{cid}");
    let full = |v: &Value| code(v) == "MEETING_CAPACITY_REACHED";

    let a = guest_in(&app, cid, "a").await;
    assert!(full(
        &app.join(cid, Auth::None, json!({ "displayName": "x" }))
            .await
            .1
    ));
    // Removal releases the seat.
    let (s, v) = app
        .post(
            &format!("{base}/participants/{}/remove", a.pid),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(occupancy(&app, cid).await, 0);
    let b = guest_in(&app, cid, "b").await;
    // Ban releases the seat.
    let (s, v) = app
        .post(
            &format!("{base}/participants/{}/ban", b.pid),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(occupancy(&app, cid).await, 0);
    let c = guest_in(&app, cid, "c").await;
    // An expired session no longer holds a seat, even before a sweep marks it EXPIRED.
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE participant_sessions SET expires_at = now() - interval '1 second' WHERE id = '{}'",
            c.session.unwrap()
        ))
        .await
        .unwrap();
    assert_eq!(occupancy(&app, cid).await, 0);
    guest_in(&app, cid, "d").await;
    assert_eq!(occupancy(&app, cid).await, 1);
    // Removed / banned participants cannot come back and take a seat.
    let (_, v) = rejoin(&app, cid, &a).await;
    assert_eq!(code(&v), "MEETING_ACCESS_DENIED", "{v}");
    let (_, v) = rejoin(&app, cid, &b).await;
    assert_eq!(code(&v), "PARTICIPANT_BANNED", "{v}");
    // End (existing two-phase semantics): LIVE → ENDING at once — no admission and no
    // seat can be taken any more; sessions are finalized when the provider confirms
    // room termination (credential-only provider here: stays ENDING, see the next test).
    let (s, v) = app
        .post(
            &format!("{base}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND status='ENDING'"
        ))
        .await,
        1
    );
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!(code(&v), "CONFERENCE_NOT_LIVE", "{v}");
}

/// J (end) — with a room-controlling provider, end reaches ENDED and finalizes every
/// live session: occupancy drops to zero.
#[tokio::test]
async fn cap_j_end_finalizes_sessions_and_occupancy() {
    let Some(app) = TestApp::livekit().await else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, open_room(3)).await;
    guest_in(&app, cid, "a").await;
    guest_in(&app, cid, "b").await;
    assert_eq!(occupancy(&app, cid).await, 2);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND status='ENDED'"
        ))
        .await,
        1,
        "{v}"
    );
    assert_eq!(occupancy(&app, cid).await, 0);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state IN ('PENDING','CONNECTED')")).await, 0);
}

/// K — the dedicated VIEWER audience pool is unchanged (admitted audience, rejoin needs
/// no new seat) and VIEWERs never consume interactive seats.
#[tokio::test]
async fn cap_k_viewer_pool_unchanged() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "town hall", "accessPolicy": "PUBLIC", "meetingType": "TOWN_HALL",
                    "interactiveCapacity": 1, "viewerCapacity": 2 }),
        )
        .await;
    let mut viewers = vec![];
    for n in ["v1", "v2"] {
        let (s, v) = app.join(cid, Auth::None, json!({ "displayName": n })).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["participant"]["role"], "VIEWER", "{v}");
        viewers.push(v["participantToken"].as_str().unwrap().to_string());
    }
    // Viewers do not occupy the (single) interactive seat.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants p WHERE p.conference_id='{cid}' AND p.role <> 'VIEWER' AND EXISTS (SELECT 1 FROM participant_sessions s WHERE s.conference_participant_id = p.id AND s.state IN ('PENDING','CONNECTED'))")).await, 0);
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "v3" }))
        .await;
    assert_eq!(
        code(&v),
        "MEETING_CAPACITY_REACHED",
        "viewer pool enforced: {v}"
    );
    // Unchanged viewer-pool semantics: the pool counts the admitted audience, so a
    // departed viewer still holds its place and can rejoin; a new viewer cannot.
    leave(&app, cid, Auth::Participant(&viewers[0]), None).await;
    let (_, v) = app
        .join(cid, Auth::None, json!({ "displayName": "v4" }))
        .await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
    let (_, v) = app
        .join(cid, Auth::Participant(&viewers[0]), json!({}))
        .await;
    assert_eq!(v["rtcStatus"], "ISSUED", "{v}");
}
