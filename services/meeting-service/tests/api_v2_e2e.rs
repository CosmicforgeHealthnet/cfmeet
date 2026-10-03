//! Phase 2 A–D system/backend E2E scenarios (spec §12) over the real HTTP router and
//! PostgreSQL. No frontend required.

mod common;

use axum::http::StatusCode;
use backend::domain::Role;
use chrono::{Duration, Utc};
use common::*;
use serde_json::json;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
    ($opts:expr) => {
        match TestApp::with($opts).await {
            Some(a) => a,
            None => return,
        }
    };
}

/// E2E 1 — MeetingSpace creation: owner from ActorContext, stable code, defaults,
/// version, no plaintext passcode anywhere.
#[tokio::test]
async fn e2e01_meeting_space_creation() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(&owner, json!({ "title": "Board", "accessPolicy": "PUBLIC_PASSCODE", "passcode": "open-sesame-42" }))
        .await;
    assert_eq!(uuid(&space["ownerUserId"]), owner.id);
    assert_eq!(space["version"], 1);
    assert_eq!(space["defaultRole"], "PARTICIPANT");
    assert_eq!(space["waitingRoomPolicy"], "OFF");
    assert_eq!(space["status"], "ACTIVE");
    assert_eq!(space["view"], "FULL");
    let code = space["meetingCode"].as_str().unwrap();
    assert!(backend::services::secrets::is_v2_meeting_code(code));
    let raw = space.to_string();
    assert!(
        !raw.contains("open-sesame-42") && !raw.contains("passcodeHash") && !raw.contains("$2")
    );
    let id = uuid(&space["id"]);
    // Stable code resolves to the same space.
    let (s, by_code) = app
        .get(
            &format!("/api/v2/meeting-spaces/{code}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(uuid(&by_code["id"]), id);
    // Stored hash is bcrypt, never plaintext.
    let plaintext = app
        .scalar(&format!("SELECT count(*) FROM meeting_spaces WHERE id = '{id}' AND passcode_hash LIKE '%open-sesame%'"))
        .await;
    assert_eq!(plaintext, 0);
    // Audit + outbox written in the same transaction.
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{id}' AND action='SPACE_CREATE'")).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE meeting_space_id='{id}' AND event_type='meeting_space.created'")).await, 1);
}

/// E2E 2 — Legacy link/id resolves to the canonical MeetingSpace.
#[tokio::test]
async fn e2e02_legacy_resolution() {
    let app = app!();
    let host = app.user("legacy-host").await;
    let legacy_id = uuid::Uuid::new_v4();
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, status) \
             VALUES ('{legacy_id}', 'ABCD2345', '{}', '{}', 'Legacy standup', false, now(), 'scheduled')",
            host.id, host.id
        ))
        .await
        .unwrap();
    use sea_orm::ConnectionTrait;
    // Created after migration → mapped lazily on first resolution.
    let (s, by_id) = app
        .get(&format!("/api/v2/resolve/{legacy_id}"), Auth::None)
        .await;
    assert_eq!(s, StatusCode::OK, "{by_id}");
    assert_eq!(by_id["matchedBy"], "LEGACY_ID");
    let space_id = uuid(&by_id["meetingSpace"]["id"]);
    assert_eq!(by_id["meetingSpace"]["meetingCode"], "ABCD2345");
    assert_eq!(by_id["meetingSpace"]["isLegacy"], true);
    assert_eq!(
        by_id["meetingSpace"]["view"], "SUMMARY",
        "resolution never implies authorization"
    );
    assert!(!by_id.to_string().contains("rtcRoom"));
    let (_, by_code) = app.get("/api/v2/resolve/abcd2345", Auth::None).await;
    assert_eq!(by_code["matchedBy"], "LEGACY_CODE");
    assert_eq!(uuid(&by_code["meetingSpace"]["id"]), space_id);
    let (_, by_space) = app
        .get(
            &format!("/api/v2/meeting-spaces/{space_id}"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(by_space["view"], "FULL", "legacy host is the owner");
    // Deterministic: a second resolution does not create another mapping.
    app.get(&format!("/api/v2/resolve/{legacy_id}"), Auth::None)
        .await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_spaces WHERE legacy_meeting_id='{legacy_id}'"
        ))
        .await,
        1
    );
    let (s, v) = app.get("/api/v2/resolve/ZZZZ9999", Auth::None).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "MEETING_NOT_FOUND"));
}

/// E2E 3 — Persistent MeetingSpace exists separately from a scheduled ConferenceSession.
#[tokio::test]
async fn e2e03_scheduled_occurrence() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(&owner, json!({ "title": "Planning" }))
        .await;
    let sid = uuid(&space["id"]);
    let start = Utc::now() + Duration::days(2);
    let (s, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": start, "scheduledEndAt": start + Duration::hours(1) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["status"], "CREATED");
    assert_eq!(c["kind"], "SCHEDULED");
    assert_ne!(c["id"], space["id"]);
    // Starting outside the start window is refused.
    let cid = uuid(&c["id"]);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
    // Space remains ACTIVE and independent of the occurrence lifecycle.
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(v["conference"]["status"], "CANCELLED");
    let (_, sp) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(sp["status"], "ACTIVE");
}

/// E2E 4 — One MeetingSpace generates multiple independent ConferenceSessions.
/// E2E 5 — Cancel/reschedule one occurrence without corrupting the series.
#[tokio::test]
async fn e2e04_05_recurring_series_and_exceptions() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "Weekly", "timezone": "Africa/Lagos" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let start_local = (Utc::now() + Duration::days(1))
        .with_timezone(&chrono_tz::Africa::Lagos)
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let (s, rule) = app
        .call(
            axum::http::Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": "FREQ=WEEKLY;COUNT=4", "startLocal": start_local, "durationMinutes": 45 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{rule}");
    assert_eq!(rule["timezone"], "Africa/Lagos");
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "bounded: COUNT=4 within horizon");
    let ids: std::collections::HashSet<_> = items.iter().map(|c| c["id"].clone()).collect();
    assert_eq!(ids.len(), 4, "independent sessions");
    for c in items {
        assert_eq!(c["kind"], "RECURRING");
        // 10:00 WAT == 09:00Z
        assert!(
            c["scheduledStartAt"]
                .as_str()
                .unwrap()
                .contains("T09:00:00"),
            "{c}"
        );
    }
    let second = uuid(&items[1]["id"]);
    let third = uuid(&items[2]["id"]);
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{second}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(v["conference"]["status"], "CANCELLED");
    assert_eq!(v["conference"]["isException"], true);
    let new_start = Utc::now() + Duration::days(30);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{third}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": new_start }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["conference"]["isException"], true);
    // Re-materialization (scheduler) is idempotent and never overwrites exceptions.
    let scheduler = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    scheduler.tick(Utc::now()).await.unwrap();
    app.state
        .meeting
        .recurrence
        .materialize_due(Utc::now() + Duration::days(40), 10)
        .await
        .unwrap();
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 4, "no duplicates after rerun");
    let cancelled = items.iter().filter(|c| c["status"] == "CANCELLED").count();
    assert_eq!(cancelled, 1);
    let still_rescheduled = items.iter().find(|c| uuid(&c["id"]) == third).unwrap();
    assert_eq!(still_rescheduled["isException"], true);
    assert_ne!(
        still_rescheduled["scheduledStartAt"],
        still_rescheduled["originalStartAt"]
    );
}

/// E2E 6 — Member joins according to access/waiting policy.
#[tokio::test]
async fn e2e06_member_access() {
    let app = app!();
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let outsider = app.user("outsider").await;
    let (sid, cid) = app
        .live_conference(&owner, json!({ "title": "Team", "accessPolicy": "INVITE_ONLY", "waitingRoomPolicy": "NON_MEMBERS_ONLY" }))
        .await;
    app.add_member(sid, &owner, &member, Role::Presenter).await;
    let (s, v) = app.join(cid, Auth::Bearer(&member.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "ADMITTED");
    assert_eq!(v["participant"]["role"], "PRESENTER");
    assert_eq!(v["permissions"]["screenSharePublish"], true);
    assert_eq!(v["rtcStatus"], "ISSUED");
    let (s, v) = app
        .join(cid, Auth::Bearer(&outsider.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
}

/// E2E 7 — Guest waits as ONE logical participant with an opaque identity and no RTC credential.
/// E2E 8 — Host admits the guest transactionally.
#[tokio::test]
async fn e2e07_08_guest_waiting_and_host_admission() {
    let app = app!();
    let host = app.user("host").await;
    let (_, cid) = app.live_conference(&host, json!({ "title": "Clinic", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" })).await;
    let issued_before = app.rtc.issued.load(std::sync::atomic::Ordering::SeqCst);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "Guest Ada" }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "WAITING");
    assert_eq!(v["participant"]["identityType"], "GUEST");
    assert!(v["rtc"].is_null());
    assert_eq!(v["rtcStatus"], "NOT_ISSUED_WAITING");
    assert!(v["participantSession"].is_null());
    assert_eq!(v["permissions"]["audioPublish"], false);
    assert_eq!(
        app.rtc.issued.load(std::sync::atomic::Ordering::SeqCst),
        issued_before,
        "no provider call while waiting"
    );
    let token = v["participantToken"].as_str().unwrap().to_string();
    let pid = uuid(&v["participant"]["id"]);
    let guest_identity: i64 = app
        .scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND guest_identity LIKE 'gst_%' AND guest_identity <> display_name"))
        .await;
    assert_eq!(guest_identity, 1, "opaque server-issued identity");
    // Polling again keeps ONE logical participant.
    let (_, again) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!(uuid(&again["participant"]["id"]), pid);
    assert!(
        again.get("participantToken").is_none(),
        "credential is issued once"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await,
        1
    );
    // Host sees and admits.
    let (_, wr) = app
        .get(
            &format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Bearer(&host.token),
        )
        .await;
    assert_eq!(wr["participants"].as_array().unwrap().len(), 1);
    let (s, r) = app
        .post(
            &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&host.token),
            json!({ "participantIds": [pid] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["changed"].as_array().unwrap().len(), 1);
    let (_, joined) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!(joined["participant"]["admissionState"], "ADMITTED");
    assert_eq!(joined["rtcStatus"], "ISSUED");
    assert!(joined["participantSession"]["id"].is_string());
}

/// E2E 9 — Bulk admission is atomic and idempotent.
#[tokio::test]
async fn e2e09_bulk_admission() {
    let app = app!();
    let host = app.user("host").await;
    let (_, cid) = app.live_conference(&host, json!({ "title": "Webinar", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" })).await;
    let mut ids = vec![];
    for i in 0..5 {
        let (_, v) = app
            .join(cid, Auth::None, json!({ "displayName": format!("g{i}") }))
            .await;
        ids.push(uuid(&v["participant"]["id"]));
    }
    let (_, r) = app
        .post(
            &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&host.token),
            json!({ "all": true }),
        )
        .await;
    assert_eq!(r["changed"].as_array().unwrap().len(), 5);
    let (_, retry) = app
        .post(
            &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&host.token),
            json!({ "participantIds": ids }),
        )
        .await;
    assert_eq!(retry["changed"].as_array().unwrap().len(), 0);
    assert_eq!(retry["unchanged"].as_array().unwrap().len(), 5);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND admission_state='ADMITTED'")).await, 5);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='participant.admitted'")).await, 5);
}

/// E2E 10 — Host of A targeting a participant of B fails, before any mutation or provider call.
#[tokio::test]
async fn e2e10_cross_conference_attack() {
    let app = app!();
    let host_a = app.user("host-a").await;
    let host_b = app.user("host-b").await;
    let (_, conf_a) = app
        .live_conference(
            &host_a,
            json!({ "title": "A", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let (_, conf_b) = app
        .live_conference(
            &host_b,
            json!({ "title": "B", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let (_, g) = app
        .join(conf_b, Auth::None, json!({ "displayName": "victim" }))
        .await;
    let victim = uuid(&g["participant"]["id"]);
    let calls_before = app
        .rtc
        .other_calls
        .load(std::sync::atomic::Ordering::SeqCst);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{conf_a}/waiting-room/admit"),
            Auth::Bearer(&host_a.token),
            json!({ "participantIds": [victim] }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "PARTICIPANT_NOT_FOUND")
    );
    let state = app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{victim}' AND admission_state='WAITING'")).await;
    assert_eq!(state, 1, "victim untouched");
    assert_eq!(
        app.rtc
            .other_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        calls_before
    );
}

/// E2E 11 — Correct passcode grants entry only; no member/host privilege.
#[tokio::test]
async fn e2e11_passcode() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "P", "accessPolicy": "PUBLIC_PASSCODE", "passcode": "4242-x" }),
        )
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "anon" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "MEETING_PASSCODE_REQUIRED")
    );
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "anon", "passcode": "nope" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "MEETING_PASSCODE_INVALID")
    );
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&user.token),
            json!({ "passcode": "4242-x" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    let (s, v) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&user.token),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/lock"),
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_members WHERE meeting_space_id='{sid}'"
        ))
        .await,
        0
    );
}

/// E2E 12 — Valid invite works; expired, revoked and exhausted fail.
#[tokio::test]
async fn e2e12_invitations() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "Private", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let invite = |body: serde_json::Value| {
        let app = &app;
        let owner = &owner;
        async move {
            let (s, v) = app
                .post(
                    &format!("/api/v2/meeting-spaces/{sid}/invitations"),
                    Auth::Bearer(&owner.token),
                    body,
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
            v
        }
    };
    let inv = invite(json!({ "useLimit": 1 })).await;
    let token = inv["token"].as_str().unwrap().to_string();
    let inv_id = uuid(&inv["invitation"]["id"]);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_invitations WHERE token_hash = '{token}'"
        ))
        .await,
        0,
        "hash only"
    );
    let (s, r) = app
        .post(
            &format!("/api/v2/invitations/{token}/resolve"),
            Auth::None,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["remainingUses"], 1, "resolve does not consume");
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "invitee", "invitationToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["identityType"], "INVITED_EXTERNAL");
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "second", "invitationToken": token }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::GONE, "INVITATION_INVALID"),
        "exhausted"
    );
    let revoked = invite(json!({})).await;
    let rid = uuid(&revoked["invitation"]["id"]);
    app.post(
        &format!("/api/v2/meeting-spaces/{sid}/invitations/{rid}/revoke"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "invitationToken": revoked["token"] }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::GONE, "INVITATION_INVALID"),
        "revoked"
    );
    let expired = invite(json!({ "expiresInHours": 1 })).await;
    let eid = uuid(&expired["invitation"]["id"]);
    app.db
        .conn
        .execute_unprepared(&format!("UPDATE meeting_invitations SET created_at = now() - interval '3 hours', expires_at = now() - interval '1 minute' WHERE id = '{eid}'"))
        .await
        .unwrap();
    use sea_orm::ConnectionTrait;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "invitationToken": expired["token"] }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_EXPIRED"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT usage_count::bigint FROM meeting_invitations WHERE id='{inv_id}'"
        ))
        .await,
        1
    );
}

/// E2E 13 — Self-promotion fails; forbidden moderator escalation fails; host/co-host follow the matrix.
#[tokio::test]
async fn e2e13_role_change() {
    let app = app!();
    let owner = app.user("owner").await;
    let cohost = app.user("cohost").await;
    let moderator = app.user("moderator").await;
    let participant = app.user("participant").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "Roles", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    app.add_member(sid, &owner, &cohost, Role::CoHost).await;
    app.add_member(sid, &owner, &moderator, Role::Moderator)
        .await;
    let mut pids = std::collections::HashMap::new();
    for (name, u) in [
        ("cohost", &cohost),
        ("moderator", &moderator),
        ("participant", &participant),
    ] {
        let (_, v) = app.join(cid, Auth::Bearer(&u.token), json!({})).await;
        pids.insert(name, uuid(&v["participant"]["id"]));
    }
    let url = |p: uuid::Uuid| format!("/api/v2/conferences/{cid}/participants/{p}/role");
    let (s, v) = app
        .patch(
            &url(pids["participant"]),
            Auth::Bearer(&participant.token),
            json!({ "role": "HOST" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "self-promotion"
    );
    let (s, v) = app
        .patch(
            &url(pids["participant"]),
            Auth::Bearer(&moderator.token),
            json!({ "role": "OWNER" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "moderator cannot mint OWNER"
    );
    let (s, v) = app
        .patch(
            &url(pids["participant"]),
            Auth::Bearer(&moderator.token),
            json!({ "role": "PRESENTER" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "moderator role change is policy-denied"
    );
    let (s, v) = app
        .patch(
            &url(pids["participant"]),
            Auth::Bearer(&cohost.token),
            json!({ "role": "CO_HOST" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
        "co-host cannot assign co-host by default"
    );
    let (s, v) = app
        .patch(
            &url(pids["participant"]),
            Auth::Bearer(&cohost.token),
            json!({ "role": "MODERATOR" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["role"], "MODERATOR");
    let (s, v) = app
        .patch(
            &url(pids["cohost"]),
            Auth::Bearer(&owner.token),
            json!({ "role": "HOST" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='ROLE_CHANGE'")).await, 2);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='participant.role_changed'")).await, 2);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.permissions_update_requested'")).await, 2);
}

/// E2E 14 — Same registered actor on two devices: 1 participant, 2 sessions.
#[tokio::test]
async fn e2e14_multi_device() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "M", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let (_, laptop) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    let (_, phone) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(laptop["participant"]["id"], phone["participant"]["id"]);
    assert_ne!(
        laptop["participantSession"]["id"],
        phone["participantSession"]["id"]
    );
    let pid = uuid(&laptop["participant"]["id"]);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE conference_id='{cid}' AND user_id='{}'", user.id)).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{pid}' AND state='PENDING'")).await, 2);
    // Reconnect supersedes one specific session.
    let (_, re) = app
        .join(
            cid,
            Auth::Bearer(&user.token),
            json!({ "previousSessionId": laptop["participantSession"]["id"] }),
        )
        .await;
    assert!(re["participantSession"]["id"].is_string());
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{pid}' AND state='REPLACED'")).await, 1);
    // Leave closes the caller's sessions only.
    let (_, left) = app
        .post(
            &format!("/api/v2/conferences/{cid}/leave"),
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(left["closedSessions"], 2);
    let attendance = backend::services::admission::attendance_seconds(
        &app.db.conn,
        cid,
        Utc::now() + Duration::minutes(1),
    )
    .await
    .unwrap();
    assert!(
        attendance.iter().any(|(p, _)| *p == pid),
        "attendance derives from session intervals"
    );
}

/// E2E 15 — Lock blocks new non-exempt entrants; unlock restores.
#[tokio::test]
async fn e2e15_lock() {
    let app = app!();
    let owner = app.user("owner").await;
    let early = app.user("early").await;
    let late = app.user("late").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "L", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    app.join(cid, Auth::Bearer(&early.token), json!({})).await;
    let (_, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/lock"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(v["conference"]["isLocked"], true);
    let (s, v) = app.join(cid, Auth::Bearer(&late.token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "CONFERENCE_LOCKED"));
    let (s, _) = app.join(cid, Auth::Bearer(&early.token), json!({})).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "admitted participant may reconnect while locked"
    );
    let (s, _) = app.join(cid, Auth::Bearer(&owner.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "owner is exempt");
    app.post(
        &format!("/api/v2/conferences/{cid}/unlock"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, _) = app.join(cid, Auth::Bearer(&late.token), json!({})).await;
    assert_eq!(s, StatusCode::OK);
}

/// E2E 16 — Ban prevents future admission and reconnect.
#[tokio::test]
async fn e2e16_ban() {
    let app = app!();
    let owner = app.user("owner").await;
    let troll = app.user("troll").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "B", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let (_, v) = app.join(cid, Auth::Bearer(&troll.token), json!({})).await;
    let pid = uuid(&v["participant"]["id"]);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/participants/{pid}/ban"),
            Auth::Bearer(&owner.token),
            json!({ "scope": "SPACE", "reasonCode": "ABUSE" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["admissionState"], "BANNED");
    assert_eq!(v["providerEnforcement"], "PENDING_PROVIDER_CONTROL");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM participant_sessions WHERE conference_participant_id='{pid}' AND state IN ('PENDING','CONNECTED')")).await, 0);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.participant_remove_requested'")).await, 1);
    let (s, v) = app.join(cid, Auth::Bearer(&troll.token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
    // Space-scoped ban also blocks the next occurrence.
    let (_, c2) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let cid2 = uuid(&c2["id"]);
    app.post(
        &format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    app.post(
        &format!("/api/v2/conferences/{cid2}/start"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app.join(cid2, Auth::Bearer(&troll.token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
}

/// E2E 17 — Concurrent starts create one LIVE conference state.
#[tokio::test]
async fn e2e17_start_concurrency() {
    let app = app!();
    let owner = app.user("owner").await;
    let host = app.user("host").await;
    let space = app.create_space(&owner, json!({ "title": "S" })).await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner, &host, Role::Host).await;
    let (_, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let cid = uuid(&c["id"]);
    let url = format!("/api/v2/conferences/{cid}/start");
    let mut handles = vec![];
    for i in 0..8 {
        let token = if i % 2 == 0 {
            owner.token.clone()
        } else {
            host.token.clone()
        };
        let router = app.router.clone();
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            use tower::ServiceExt;
            let req = axum::http::Request::post(&url)
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap();
            let resp = router.oneshot(req).await.unwrap();
            let status = resp.status();
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap();
            (status, body["changed"].as_bool())
        }));
    }
    let mut changed = 0;
    for h in handles {
        let (s, c) = h.await.unwrap();
        assert_eq!(s, StatusCode::OK);
        if c == Some(true) {
            changed += 1;
        }
    }
    assert_eq!(changed, 1, "exactly one start wins; others are idempotent");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='LIVE'")).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='conference.started'")).await, 1);
}

/// E2E 18 — When ENDING wins, new join authorization fails closed.
#[tokio::test]
async fn e2e18_join_end_race() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "R", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["conference"]["status"], "ENDING", "never faked as ENDED");
    assert_eq!(v["providerEnforcement"], "PENDING_PROVIDER_CONTROL");
    let issued = app.rtc.issued.load(std::sync::atomic::Ordering::SeqCst);
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    assert_eq!(
        app.rtc.issued.load(std::sync::atomic::Ordering::SeqCst),
        issued,
        "no credential after ENDING"
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND event_type='rtc.room_terminate_requested'")).await, 1);
}
