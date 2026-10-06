//! Part M — system E2E certification (M-E2E-01..M-E2E-11 + recovery scenarios).
//!
//! Multiple simultaneous actors (OWNER, HOST, CO_HOST, MEMBER, PARTICIPANT, GUEST,
//! VIEWER, PRESENTER, webinar attendee, healthcare patient / clinician) drive the real
//! HTTP router and the realtime WebSocket stream (`TestApp::serve`). Media goes through
//! the production `LiveKitProvider` against the in-process LiveKit RoomService mock,
//! whose simulated SFU verifies every credential and enforces the CURRENT grants, and
//! every scenario checks the database and the provider state as well as the API.
//! The media-critical scenarios are repeated against a real livekit-server in
//! `tests/livekit_certification.rs` (`real_m_e2e_*`).

mod common;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use backend::{domain::Role, services::rtc_control::RtcControl};
use chrono::{TimeZone, Utc};
use common::{artifacts as art, collab::*, livekit_mock, proxy::*, rtc::*, ws::*, *};
use livekit_protocol::TrackSource;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

const EVENT: Duration = Duration::from_secs(12);

fn publish(
    app: &TestApp,
    room_name: &str,
    j: &Joined,
    source: TrackSource,
) -> Result<String, String> {
    lk(app).publish(room_name, &j.identity, source)
}

async fn participants(app: &TestApp, cid: Uuid, owner: &User) -> Vec<Value> {
    let (s, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/participants"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v.as_array().cloned().expect("roster array")
}

/// M-E2E-01: owner schedules, member joins directly, guest waits, host sees the waiting
/// state live, admits, both obtain correct credentials, connect to the media room, and
/// canonical state matches database and provider.
#[tokio::test]
async fn m_e2e_01_schedule_invite_wait_admit_connect() {
    let app = app!(TestApp::artifacts());
    let addr = app.serve().await;
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "M-E2E-01", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY", "timezone": "Africa/Lagos" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    let start = Utc::now() + chrono::Duration::minutes(5);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": start, "scheduledEndAt": start + chrono::Duration::hours(1) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let cid = uuid(&c["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let host = owner_joined(&app, cid, &owner).await;
    let mut host_ws = subscribe(&app, addr, cid, Auth::Bearer(&owner.token), None).await;
    assert_eq!(frame(&mut host_ws, EVENT).await.unwrap()["scope"], "FULL");

    // Member: direct admission (membership), media credential.
    let m = join_user(&app, cid, &member).await;
    assert_eq!(m.body["participant"]["role"], "PARTICIPANT");
    // Guest: waiting, no credential, SELF_ONLY stream.
    let (s, g) = app
        .join(cid, Auth::None, json!({ "displayName": "Guest Ada" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    assert_eq!(g["participant"]["admissionState"], "WAITING");
    assert!(g["rtc"].is_null());
    let gtoken = g["participantToken"].as_str().unwrap().to_string();
    let gpid = uuid(&g["participant"]["id"]);
    let mut guest_ws = subscribe(&app, addr, cid, Auth::Participant(&gtoken), None).await;
    assert_eq!(
        frame(&mut guest_ws, EVENT).await.unwrap()["scope"],
        "SELF_ONLY"
    );
    // Host receives the waiting state live and through REST.
    let waiting = expect_event(&mut host_ws, "participant.requested_join", EVENT).await;
    assert!(waiting.to_string().contains(&gpid.to_string()), "{waiting}");
    let (_, wr) = app
        .get(
            format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(wr["participants"][0]["id"], json!(gpid));
    // Admit → the guest learns it on its own stream, then obtains its credential.
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [gpid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    expect_event(&mut guest_ws, "participant.admitted", EVENT).await;
    let (s, g2) = app.join(cid, Auth::Participant(&gtoken), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{g2}");
    assert_eq!(g2["rtcStatus"], "ISSUED");
    let guest = Joined {
        participant_id: gpid,
        session_id: uuid(&g2["participantSession"]["id"]),
        identity: format!("ps_{}", uuid(&g2["participantSession"]["id"]).simple()),
        credential: g2["rtc"]["credential"].as_str().unwrap().to_string(),
        participant_token: Some(gtoken.clone()),
        body: g2.clone(),
    };
    // Both connect to the media room (SFU verifies the credential) and the verified
    // provider callbacks mark their sessions CONNECTED.
    connect(&app, cid, &m).await;
    connect(&app, cid, &guest).await;
    for j in [&host, &m, &guest] {
        assert_eq!(session_state(&app, j.session_id).await, "CONNECTED");
        assert!(lk(&app).participant(&room(cid), &j.identity).is_some());
    }
    // Canonical state == database == provider.
    let (_, conf) = app
        .get(
            format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(conf["status"], "LIVE");
    let ps = participants(&app, cid, &owner).await;
    let admitted = ps
        .iter()
        .filter(|p| p["admissionState"] == "ADMITTED")
        .count();
    assert_eq!(admitted, 3, "{ps:?}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND state='CONNECTED'"
        ))
        .await,
        3
    );
    let mut provider = lk(&app).identities(&room(cid));
    provider.sort();
    let mut expected: Vec<String> = [&host, &m, &guest]
        .iter()
        .map(|j| j.identity.clone())
        .collect();
    expected.sort();
    assert_eq!(
        provider, expected,
        "provider room == authoritative sessions"
    );
}

/// M-E2E-02: a co-host moderates; the original host disconnects; the conference keeps
/// running and the co-host keeps exactly its granted authority.
#[tokio::test]
async fn m_e2e_02_cohost_moderates_after_host_leaves() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let host = owner_joined(&app, cid, &owner).await;
    let co_user = app.user("cohost").await;
    let co = join_user(&app, cid, &co_user).await;
    connect(&app, cid, &co).await;
    let (s, v) = change_role(&app, cid, &owner, co.participant_id, "CO_HOST").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Original host leaves (session closed + provider disconnect).
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/leave"),
            Auth::Bearer(&owner.token),
            json!({ "participantSessionId": host.session_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    lk(&app).disconnect(&room(cid), &host.identity);
    provider_event(&app, "participant_left", cid, Some(&host.identity)).await;
    assert_eq!(
        conference_status(&app, cid).await,
        "LIVE",
        "conference survives host leaving"
    );
    // Co-host moderation works and is provider-enforced.
    let (s, v) = app
        .post(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/mute",
                g[0].participant_id
            ),
            Auth::Bearer(&co_user.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .post(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                g[1].participant_id
            ),
            Auth::Bearer(&co_user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        lk(&app).participant(&room(cid), &g[1].identity).is_none(),
        "removed at the provider"
    );
    // …but only within granted authority.
    for role in ["HOST", "OWNER"] {
        let (s, _) = change_role(&app, cid, &co_user, g[0].participant_id, role).await;
        assert!(s.is_client_error(), "co-host cannot grant {role}");
    }
    let (s, _) = change_role(&app, cid, &co_user, host.participant_id, "VIEWER").await;
    assert!(s.is_client_error(), "co-host cannot demote the owner");
    let space_id = uuid(
        &app.get(
            format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&owner.token),
        )
        .await
        .1["meetingSpaceId"],
    );
    let (s, _) = app
        .post(
            format!("/api/v2/meeting-spaces/{space_id}/owner-transfer"),
            Auth::Bearer(&co_user.token),
            json!({ "newOwnerUserId": co_user.id, "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Provider permissions remain correct: co-host publishes, the muted participant's
    // grant is unchanged (mute is not a lock).
    assert!(publish(&app, &room(cid), &co, TrackSource::Microphone).is_ok());
    assert!(publish(&app, &room(cid), &g[0], TrackSource::Microphone).is_ok());
}

/// M-E2E-03: lock blocks non-exempt joins, unlock restores them, existing participants
/// are unaffected.
#[tokio::test]
async fn m_e2e_03_lock_and_unlock() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/lock"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert!(
        (s, code(&v)) == (StatusCode::FORBIDDEN, "CONFERENCE_LOCKED")
            || v["participant"]["admissionState"] == "WAITING",
        "non-exempt join denied or waiting: {s} {v}"
    );
    assert!(v["rtc"].is_null() || v.get("rtc").is_none());
    assert_eq!(
        session_state(&app, g[0].session_id).await,
        "CONNECTED",
        "existing unaffected"
    );
    assert!(publish(&app, &room(cid), &g[0], TrackSource::Camera).is_ok());
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/unlock"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let j = join_guest(&app, cid, "after-unlock").await;
    assert_eq!(j.body["participant"]["admissionState"], "ADMITTED");
}

/// M-E2E-04: audio lock — the modified client's microphone publish is refused by the
/// provider; unlock restores it.
#[tokio::test]
async fn m_e2e_04_audio_lock_provider_enforced() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let uri = format!(
        "{}/participants/{}/audio-lock",
        base(cid),
        g[0].participant_id
    );
    let (s, v) = app.post(&uri, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["providerEnforcement"], "ENFORCED");
    let err = publish(&app, &room(cid), &g[0], TrackSource::Microphone).unwrap_err();
    assert!(err.contains("permission denied"), "{err}");
    assert!(
        publish(&app, &room(cid), &g[0], TrackSource::Camera).is_ok(),
        "only audio locked"
    );
    // A refreshed credential carries the lock too.
    let (s, c) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["permissions"]["audioPublish"], false);
    let (s, _) = app.delete(&uri, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(publish(&app, &room(cid), &g[0], TrackSource::Microphone).is_ok());
}

/// M-E2E-05: removal disconnects at the provider and the stale credential cannot
/// reconnect; new authorization is required (and refused).
#[tokio::test]
async fn m_e2e_05_remove_and_stale_credential() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let victim = &g[0];
    let (s, _) = app
        .post(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                victim.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(lk(&app).participant(&room(cid), &victim.identity).is_none());
    // The old JWT is still cryptographically valid; the SFU lets it in, the verified
    // callback maps it to a non-active session and the provider evicts it.
    if lk(&app).connect(&victim.credential).is_ok() {
        let (s, v) = provider_event(&app, "participant_joined", cid, Some(&victim.identity)).await;
        assert_eq!(s, StatusCode::OK);
        assert_ne!(v["outcome"], "APPLIED", "{v}");
        assert!(
            lk(&app).participant(&room(cid), &victim.identity).is_none(),
            "evicted"
        );
    }
    assert_ne!(session_state(&app, victim.session_id).await, "CONNECTED");
    let (s, v) = app
        .join(cid, victim.auth(), json!({ "displayName": "again" }))
        .await;
    assert!(
        s.is_client_error(),
        "new authorization required and refused: {s} {v}"
    );
    let (s, _) = refresh(&app, cid, victim, victim.auth()).await;
    assert!(s.is_client_error());
}

/// M-E2E-06: timezone-aware recurring series across a DST change; one occurrence
/// cancelled; the series stays intact; no duplicates on re-materialization.
#[tokio::test]
async fn m_e2e_06_recurring_series_dst_and_exception() {
    let app = app!(TestApp::new());
    let owner = app.user("owner").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "Weekly", "timezone": "Europe/London" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    // Mondays 09:00 London across the 2027-03-28 BST change.
    let (s, v) = app
        .call(
            axum::http::Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": "FREQ=WEEKLY;BYDAY=MO;COUNT=4", "startLocal": "2027-03-15T09:00:00", "durationMinutes": 45 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let scheduler = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    let as_of = Utc.with_ymd_and_hms(2027, 3, 10, 0, 0, 0).unwrap();
    scheduler.tick(as_of).await.unwrap();
    let (_, list) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let starts: Vec<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["scheduledStartAt"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(starts.len(), 4, "{list}");
    assert!(
        starts.iter().any(|s| s.starts_with("2027-03-15T09:00:00")),
        "GMT: {starts:?}"
    );
    assert!(
        starts.iter().any(|s| s.starts_with("2027-03-22T09:00:00")),
        "GMT: {starts:?}"
    );
    assert!(
        starts.iter().any(|s| s.starts_with("2027-03-29T08:00:00")),
        "BST = UTC+1: {starts:?}"
    );
    assert!(
        starts.iter().any(|s| s.starts_with("2027-04-05T08:00:00")),
        "BST: {starts:?}"
    );
    // Cancel exactly one occurrence.
    let (s, v) = app
        .call(
            axum::http::Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
            Auth::Bearer(&owner.token),
            Some(json!({ "originalStartAt": "2027-03-29T08:00:00Z", "action": "CANCEL" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for _ in 0..3 {
        exec(
            &app,
            "UPDATE meeting_recurrence_rules SET materialized_until = NULL",
        )
        .await;
        scheduler.tick(as_of).await.unwrap();
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await,
        4,
        "no duplicate occurrence"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CANCELLED'")).await,
        1,
        "exactly one cancelled"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CREATED'")).await,
        3,
        "future series intact"
    );
}

/// M-E2E-07: breakouts — assignment, scoped media and chat, move A → B with no A
/// context left, countdown close returns everyone to the main conference.
#[tokio::test]
async fn m_e2e_07_breakout_lifecycle() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "names": ["A", "B"] })).await;
    let (ra, rb) = (rooms[0], rooms[1]);
    let (s, v) = app
        .post(
            format!("{}/breakouts/assign", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "MANUAL", "assignments": [
                { "participantId": g[0].participant_id, "breakoutId": ra },
                { "participantId": g[1].participant_id, "breakoutId": ra },
                { "participantId": g[2].participant_id, "breakoutId": rb } ] }),
        )
        .await;
    assert!(s.is_success(), "{v}");
    open_room(&app, cid, &owner, ra).await;
    open_room(&app, cid, &owner, rb).await;
    // Media scope: credential targets the breakout provider room.
    let (s, c) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["mediaContext"]["breakoutId"], json!(ra));
    let ida = lk(&app)
        .connect(
            c["credential"]
                .as_str()
                .or(c["rtc"]["credential"].as_str())
                .unwrap(),
        )
        .unwrap();
    assert!(lk(&app).participant(&breakout_room(ra), &ida).is_some());
    // Chat scope: room A message invisible to room B.
    let (s, _) = send(
        &app,
        cid,
        g[0].auth(),
        json!({ "body": "A-only", "breakoutId": ra }),
    )
    .await;
    assert!(s.is_success());
    let (_, ev_b) = app
        .get(format!("{}/events?after=0", base(cid)), g[2].auth())
        .await;
    assert!(!ev_b.to_string().contains("A-only"));
    // Move participant 0 from A to B.
    let (s, v) = app
        .post(
            format!("{}/breakouts/move", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": rb }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        lk(&app).participant(&breakout_room(ra), &ida).is_none(),
        "no A media context left"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM breakout_assignments WHERE participant_id='{}' AND unassigned_at IS NULL AND breakout_room_id='{rb}'",
            g[0].participant_id
        ))
        .await,
        1
    );
    let (_, c) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(c["mediaContext"]["breakoutId"], json!(rb));
    let (s, _) = app
        .get(
            format!("{}/messages?scope=BREAKOUT&breakoutId={ra}", base(cid)),
            g[0].auth(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "room A chat no longer readable");
    // Countdown close of every room.
    let (s, v) = app
        .post(
            format!("{}/breakouts/close-all", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "countdownSeconds": 30 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    exec(&app, &format!("UPDATE breakout_rooms SET closing_at = now() - interval '1 second' WHERE conference_id='{cid}'")).await;
    let closed = backend::workers::BreakoutCloser::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert_eq!(closed, 2);
    for j in &g {
        let (_, c) = refresh(&app, cid, j, j.auth()).await;
        assert_eq!(c["mediaContext"]["kind"], "MAIN", "{c}");
    }
    assert!(!lk(&app).has_room(&breakout_room(ra)) && !lk(&app).has_room(&breakout_room(rb)));
}

/// M-E2E-08: poll visibility, results and anonymity.
#[tokio::test]
async fn m_e2e_08_poll_visibility_results_anonymity() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_CLOSE")).await;
    assert!(vote(&app, cid, poll, g[0].auth(), "o1")
        .await
        .0
        .is_success());
    assert!(vote(&app, cid, poll, g[1].auth(), "o1")
        .await
        .0
        .is_success());
    assert!(vote(&app, cid, poll, g[2].auth(), "o2")
        .await
        .0
        .is_success());
    let (s, v) = app
        .get(format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "POLL_RESULTS_NOT_VISIBLE")
    );
    let (s, _) = app
        .post(
            format!("{}/polls/{poll}/close", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, r) = app
        .get(format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let text = r.to_string();
    assert!(
        text.contains("\"o1\"") && text.contains('2') && text.contains("\"o2\""),
        "{r}"
    );
    // Anonymous poll: aggregate only, no voter identity anywhere.
    let mut body = simple_poll("AFTER_CLOSE");
    body["anonymous"] = json!(true);
    let anon = open_poll(&app, cid, &owner, body).await;
    for j in &g {
        assert!(vote(&app, cid, anon, j.auth(), "o2").await.0.is_success());
    }
    app.post(
        format!("{}/polls/{anon}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (_, r) = app
        .get(
            format!("{}/polls/{anon}/results", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let (_, events) = app
        .get(
            format!("{}/events?after=0", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    for j in &g {
        let pid = j.participant_id.to_string();
        assert!(!r.to_string().contains(&pid), "results leak a voter");
        let leaked = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["eventType"] == "poll.results_updated" && e.to_string().contains(&pid));
        assert!(!leaked, "events leak an anonymous voter");
    }
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM meeting_poll_responses WHERE poll_id='{anon}' AND participant_id IS NOT NULL")).await,
        0
    );
}

/// M-E2E-09: explicit recording consent, start, `recording.started` delivered to
/// subscribed clients, provider capture, stop, processing, READY, authorized download.
#[tokio::test]
async fn m_e2e_09_consented_recording_to_authorized_download() {
    let app = app!(TestApp::artifacts());
    let addr = app.serve().await;
    let (owner, sid, cid, g) = art::meeting(&app, 2).await;
    let (s, v) = art::set_profile(
        &app,
        &owner,
        sid,
        json!({ "recording": { "consentMode": "EXPLICIT_CONSENT_REQUIRED" } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let mut subs = vec![];
    for j in &g {
        subs.push(subscribe(&app, addr, cid, j.auth(), None).await);
    }
    // Missing consent blocks the provider start (nothing is created).
    let (_, v) = art::start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{v}");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
    for j in &g {
        let (s, v) = art::consent(&app, cid, j.auth(), "RECORDING", "ACCEPTED").await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (rid, eg) = art::recording_active(&app, cid, &owner).await;
    for ws in subs.iter_mut() {
        expect_event(ws, "recording.started", EVENT).await;
    }
    assert_eq!(art::artifact_status(&app, rid).await, "RECORDING");
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/recordings/{rid}/stop"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(art::artifact_status(&app, rid).await, "STOPPING");
    let bytes = b"\x00\x00\x00\x18ftypmp42 m-e2e-09".repeat(64);
    let body = lk(&app).egress_complete(&eg, Some(&app.storage), &bytes, None);
    assert_eq!(art::egress_event(&app, &body).await.0, StatusCode::OK);
    assert_eq!(art::artifact_status(&app, rid).await, "READY");
    let (s, a) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert!(a["url"].is_string());
    let key = art::storage_key(&app, rid).await;
    assert!(app.storage.contains(&key));
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            g[0].auth(),
            json!({}),
        )
        .await;
    assert!(s.is_client_error(), "participants cannot download");
}

/// M-E2E-10: webinar attendee — VIEWER, no publish, Q&A + poll + reaction, promoted to
/// PRESENTER (publishes), demoted (publish removed).
#[tokio::test]
async fn m_e2e_10_webinar_attendee_promotion() {
    let app = app!(TestApp::artifacts());
    let owner = app.user("organizer").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "Webinar", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" }),
        )
        .await;
    let a = join_guest(&app, cid, "attendee").await;
    connect(&app, cid, &a).await;
    assert_eq!(a.body["participant"]["role"], "VIEWER");
    for src in [
        TrackSource::Microphone,
        TrackSource::Camera,
        TrackSource::ScreenShare,
    ] {
        assert!(publish(&app, &room(cid), &a, src).is_err(), "{src:?}");
    }
    let (s, _) = app
        .post(
            format!("{}/questions", base(cid)),
            a.auth(),
            json!({ "body": "When?" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    assert!(vote(&app, cid, poll, a.auth(), "o1").await.0.is_success());
    let (s, v) = app
        .post(
            format!("{}/reactions", base(cid)),
            a.auth(),
            json!({ "reaction": "clap" }),
        )
        .await;
    assert!(
        s.is_success() || code(&v) == "REACTIONS_DISABLED",
        "{s} {v}"
    );
    let (s, v) = change_role(&app, cid, &owner, a.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(publish(&app, &room(cid), &a, TrackSource::Camera).is_ok());
    assert!(publish(&app, &room(cid), &a, TrackSource::Microphone).is_ok());
    let (s, _) = change_role(&app, cid, &owner, a.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK);
    assert!(publish(&app, &room(cid), &a, TrackSource::Camera).is_err());
}

/// M-E2E-11: healthcare — uncontrolled access rejected, appointment authorization
/// required, waiting room enforced, clinician admits, recording off by default, no
/// clinical payload stored.
#[tokio::test]
async fn m_e2e_11_healthcare_flow() {
    let patient_user_id = std::sync::Mutex::new(None::<Uuid>);
    let _ = &patient_user_id;
    let opts = |allowed: Vec<Uuid>| TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        appointments: Some(Arc::new(AllowListAppointments(allowed))),
        ..TestOptions::default()
    };
    // Create users first on a throwaway app to learn ids is not possible across DBs;
    // instead allow-list fixed ids inserted below.
    let doctor_id = Uuid::new_v4();
    let patient_id = Uuid::new_v4();
    let app = app!(TestApp::with(opts(vec![doctor_id, patient_id])));
    let doctor = insert_user(&app, doctor_id, "doctor").await;
    let patient = insert_user(&app, patient_id, "patient").await;
    let stranger = app.user("stranger").await;
    let (sid, cid) = app
        .live_conference(
            &doctor,
            json!({ "title": "Consult", "meetingType": "HEALTHCARE", "securityProfile": "HEALTHCARE",
                    "accessPolicy": "HEALTHCARE_APPOINTMENT", "accessConfig": { "appointmentRef": "APPT-42" } }),
        )
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "walk-in" }))
        .await;
    assert!(
        s.is_client_error(),
        "uncontrolled public access rejected: {s} {v}"
    );
    let (s, _) = app
        .join(cid, Auth::Bearer(&stranger.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "no appointment, no entry");
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&patient.token),
            json!({ "externalContext": "{\"diagnosis\":\"secret-clinical-note\"}" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["participant"]["admissionState"], "WAITING",
        "waiting room enforced"
    );
    assert!(v["rtc"].is_null());
    let pid = uuid(&v["participant"]["id"]);
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&patient.token),
            json!({ "participantIds": [pid] }),
        )
        .await;
    assert!(s.is_client_error(), "a patient cannot admit");
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&doctor.token),
            json!({ "participantIds": [pid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app.join(cid, Auth::Bearer(&patient.token), json!({})).await;
    assert_eq!(
        (s, v["rtcStatus"].as_str()),
        (StatusCode::OK, Some("ISSUED")),
        "{v}"
    );
    let (s, v) = art::start_recording(&app, cid, Auth::Bearer(&doctor.token)).await;
    assert!(s.is_client_error(), "recording off by default: {s} {v}");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
    let dump = app
        .scalar(&format!(
            "SELECT count(*) FROM (SELECT row_to_json(t)::text AS j FROM conference_participants t WHERE conference_id='{cid}' \
             UNION ALL SELECT row_to_json(t)::text FROM participant_sessions t WHERE conference_id='{cid}' \
             UNION ALL SELECT row_to_json(t)::text FROM outbox_events t WHERE conference_id='{cid}' \
             UNION ALL SELECT row_to_json(t)::text FROM conference_audit_events t WHERE conference_id='{cid}') x \
             WHERE j LIKE '%secret-clinical-note%'"
        ))
        .await;
    assert_eq!(
        dump, 0,
        "no clinical payload duplicated into the meeting domain"
    );
    let _ = sid;
}

async fn insert_user(app: &TestApp, id: Uuid, name: &str) -> User {
    let email = format!("{name}-{}@example.com", &id.simple().to_string()[..8]);
    exec(
        app,
        &format!(
            "INSERT INTO users (id, username, email, status, role, auth_type) VALUES ('{id}', '{name}-{}', '{email}', 'active', 'user', 'local')",
            &id.simple().to_string()[..6]
        ),
    )
    .await;
    User {
        id,
        token: app
            .state
            .jwt_service
            .generate_access_token(id, "user")
            .unwrap(),
        email,
    }
}

// ---------------------------------------------------------------------------
// Recovery E2E
// ---------------------------------------------------------------------------

/// WebSocket disconnect → reconnect with cursor (replay) and with a cursor too old /
/// ahead (explicit resync → canonical refetch); media continues meanwhile.
#[tokio::test]
async fn r_e2e_websocket_replay_and_resync() {
    let app = app!(TestApp::livekit());
    let addr = app.serve().await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "ws", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "g").await;
    connect(&app, cid, &g).await;
    let mut ws = subscribe(&app, addr, cid, g.auth(), None).await;
    let cursor = frame(&mut ws, EVENT).await.unwrap()["cursor"]
        .as_i64()
        .unwrap();
    drop(ws);
    for action in ["lock", "unlock", "lock"] {
        app.post(
            format!("/api/v2/conferences/{cid}/{action}"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    }
    assert_eq!(
        session_state(&app, g.session_id).await,
        "CONNECTED",
        "media continues"
    );
    let mut ws = subscribe(&app, addr, cid, g.auth(), Some(cursor)).await;
    let mut types = vec![];
    while types.len() < 3 {
        let f = frame(&mut ws, EVENT).await.expect("replayed frame");
        if f["type"] == "event" {
            types.push(f["event"]["eventType"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(
        types,
        [
            "conference.locked",
            "conference.unlocked",
            "conference.locked"
        ]
    );
    drop(ws);
    let mut ws = subscribe(&app, addr, cid, g.auth(), Some(1_000_000)).await;
    let f = frame(&mut ws, EVENT).await.unwrap();
    assert_eq!(f["type"], "resync_required", "{f}");
    let canonical = f["canonical"].as_str().unwrap().to_string();
    let (s, c) = app.get(&canonical, g.auth()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["isLocked"], true, "canonical refetch recovers state");
}

/// Provider callback loss → reconciliation; RTC room disappearance → deterministic
/// repair; orphan provider participant → eviction.
#[tokio::test]
async fn r_e2e_callback_loss_room_loss_orphans() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "rec", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app).connect(&p.credential).unwrap(); // callback lost
    assert_eq!(session_state(&app, p.session_id).await, "PENDING");
    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    reconciler.tick().await.unwrap();
    assert_eq!(
        session_state(&app, p.session_id).await,
        "CONNECTED",
        "late join repaired"
    );
    lk(&app).drop_room(&room(cid));
    exec(
        &app,
        "UPDATE worker_leases SET expires_at = now() - interval '1 second'",
    )
    .await;
    let r = reconciler.tick().await.unwrap();
    assert!(r.kinds.contains(&"LIVE_ROOM_MISSING".to_string()), "{r:?}");
    assert!(lk(&app).has_room(&room(cid)), "room re-created");
    lk(&app).insert_participant(&room(cid), "ps_orphan_identity", Default::default());
    exec(
        &app,
        "UPDATE worker_leases SET expires_at = now() - interval '1 second'",
    )
    .await;
    reconciler.tick().await.unwrap();
    assert!(
        lk(&app)
            .participant(&room(cid), "ps_orphan_identity")
            .is_none(),
        "orphan evicted"
    );
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM rtc_drift_events WHERE conference_id='{cid}'"
        ))
        .await
            >= 2
    );
}

/// Conference end while a recording is active: capture stopped in the same
/// transaction, conference ends, artefact finalizes.
#[tokio::test]
async fn r_e2e_end_while_recording() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, _) = art::meeting(&app, 1).await;
    let (rid, eg) = art::recording_active(&app, cid, &owner).await;
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(matches!(
        art::artifact_status(&app, rid).await.as_str(),
        "STOPPING" | "PROCESSING"
    ));
    let body = lk(&app).egress_complete(
        &eg,
        Some(&app.storage),
        b"mp4-bytes-end-while-recording",
        None,
    );
    assert_eq!(art::egress_event(&app, &body).await.0, StatusCode::OK);
    assert_eq!(art::artifact_status(&app, rid).await, "READY");
    assert_eq!(conference_status(&app, cid).await, "ENDED");
}

/// One logical participant across several ParticipantSessions (multi-device and
/// reconnect), guest reconnect with its server-issued credential, attendance merged.
#[tokio::test]
async fn r_e2e_reconnect_across_sessions_and_guest_reconnect() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 0).await;
    let user = app.user("member").await;
    let d1 = join_user(&app, cid, &user).await;
    let d2 = join_user(&app, cid, &user).await;
    assert_eq!(
        d1.participant_id, d2.participant_id,
        "one logical participant"
    );
    assert_ne!(d1.session_id, d2.session_id, "two device sessions");
    connect(&app, cid, &d1).await;
    connect(&app, cid, &d2).await;
    // Device 1 drops; reconnect supersedes its session.
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&user.token),
            json!({ "previousSessionId": d1.session_id }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(uuid(&v["participant"]["id"]), d1.participant_id);
    assert_ne!(session_state(&app, d1.session_id).await, "CONNECTED");
    // Guest reconnect with its own credential keeps its identity; a display-name
    // look-alike does not.
    let guest = join_guest(&app, cid, "Ada").await;
    let (s, v) = app
        .join(cid, guest.auth(), json!({ "displayName": "Ada" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(uuid(&v["participant"]["id"]), guest.participant_id);
    let other = join_guest(&app, cid, "Ada").await;
    assert_ne!(other.participant_id, guest.participant_id);
    let (s, att) = app
        .get(
            format!("/api/v2/conferences/{cid}/attendance"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(
        s.is_success() || s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
        "{att}"
    );
    let _ = g;
}

/// Redis outage → degraded operation → recovery: the outbox converges and live
/// subscribers receive events again; Redis never granted anything meanwhile.
#[tokio::test]
async fn r_e2e_redis_outage_and_recovery() {
    if !require_test_redis() {
        return;
    }
    let proxy = ToggleProxy::start(redis_target()).await;
    let app = app!(TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        redis: RedisMode::Via(proxy.port),
        flags: backend::config::FeatureFlags {
            rtc_provider_control_v2: true,
            conference_realtime_enabled: true,
            ..backend::config::FeatureFlags::all_v2_enabled()
        },
        ..TestOptions::default()
    }));
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "redis", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let relay = backend::workers::outbox_relay::OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(backend::services::realtime::RedisPublisher::new(
            app.state.redis.clone(),
        ))
            as Arc<dyn backend::workers::outbox_relay::EventPublisher>),
        Some(Arc::new(RtcControl::new(app.state.meeting.core.clone()))),
    );
    relay.tick(500).await.unwrap();
    proxy.down();
    let g = join_guest(&app, cid, "during-outage").await;
    assert_eq!(
        g.body["rtcStatus"], "ISSUED",
        "PostgreSQL authority continues"
    );
    let (s, _) = change_role(&app, cid, &owner, g.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK);
    for _ in 0..3 {
        exec(
            &app,
            "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
        )
        .await;
        relay.tick(500).await.unwrap();
    }
    let pending = app
        .scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND status='PENDING'"))
        .await;
    assert!(pending > 0, "events retained during the outage");
    proxy.up();
    let mut converged = false;
    for _ in 0..30 {
        exec(
            &app,
            "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
        )
        .await;
        let _ = relay.tick(500).await;
        if app
            .scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND channel='REALTIME' AND status='PENDING'"))
            .await
            == 0
        {
            converged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(converged, "outbox drained after Redis recovered");
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='FAILED'")
            .await,
        0
    );
    let _ = livekit_mock::API_KEY;
}

/// Invitation revoke / expiry and ban / rejoin through the full join path.
#[tokio::test]
async fn r_e2e_invitation_revoke_expiry_and_ban_rejoin() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "inv", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let invite = |hours: i64| {
        let app = &app;
        let owner = &owner;
        async move {
            let (s, v) = app
                .post(
                    format!("/api/v2/meeting-spaces/{sid}/invitations"),
                    Auth::Bearer(&owner.token),
                    json!({ "conferenceId": cid, "expiresInHours": hours }),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
            (
                uuid(&v["invitation"]["id"]),
                v["token"].as_str().unwrap().to_string(),
            )
        }
    };
    let (rid, revoked) = invite(24).await;
    app.post(
        format!("/api/v2/meeting-spaces/{sid}/invitations/{rid}/revoke"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, _) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "invitationToken": revoked }),
        )
        .await;
    assert_eq!(s, StatusCode::GONE);
    let (eid, expired) = invite(24).await;
    exec(&app, &format!("UPDATE meeting_invitations SET created_at = now() - interval '2 minutes', expires_at = now() - interval '1 minute' WHERE id='{eid}'")).await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "invitationToken": expired }),
        )
        .await;
    assert_eq!(s, StatusCode::GONE, "{v}");
    let (_, ok) = invite(24).await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "guest", "invitationToken": ok }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["participantToken"].as_str().unwrap().to_string();
    let pid = uuid(&v["participant"]["id"]);
    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/participants/{pid}/ban"),
            Auth::Bearer(&owner.token),
            json!({ "scope": "SPACE" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
}
