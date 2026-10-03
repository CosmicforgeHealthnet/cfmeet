//! Permanent Phase 2 security regression suite (spec §8, items 1–22).

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use backend::domain::Role;
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

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

async fn guest(app: &TestApp, cid: Uuid, name: &str) -> (Uuid, String) {
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": name }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (
        uuid(&v["participant"]["id"]),
        v["participantToken"].as_str().unwrap().to_string(),
    )
}

/// 1. An unauthenticated caller cannot submit another user's user_id and become them.
#[tokio::test]
async fn s01_no_user_id_impersonation() {
    let app = app!();
    let owner = app.user("owner").await;
    let victim = app.user("victim").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "userId": victim.id }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::None,
            json!({ "title": "x" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
    // Forged token for the victim (wrong signing key) is rejected, not downgraded.
    let forged = backend::services::auth::JwtService::new("attacker".into())
        .generate_access_token(victim.id, "user")
        .unwrap();
    let (s, v) = app.join(cid, Auth::Bearer(&forged), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
    // A legacy guest JWT whose subject is a user id never becomes that user in v2.
    let legacy_guest = app
        .state
        .jwt_service
        .generate_access_token(victim.id, "guest")
        .unwrap();
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&legacy_guest),
            json!({ "displayName": "g" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["participant"]["identityType"], "GUEST");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE user_id='{}'",
            victim.id
        ))
        .await,
        0
    );
}

/// 2. A client cannot submit host/owner role (or admission state) and gain privilege.
#[tokio::test]
async fn s02_no_client_supplied_role() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let (_, cid) = app.live_conference(&owner, json!({ "title": "x", "accessPolicy": "AUTHENTICATED", "waitingRoomPolicy": "EVERYONE" })).await;
    for body in [
        json!({ "role": "HOST" }),
        json!({ "owner": true }),
        json!({ "admissionState": "ADMITTED" }),
        json!({ "host": true }),
    ] {
        let (s, v) = app.join(cid, Auth::Bearer(&user.token), body).await;
        assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    }
    let (_, v) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    assert_eq!(v["participant"]["admissionState"], "WAITING");
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&user.token),
            json!({ "title": "t", "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let (s, v) = app
        .post(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&user.token),
            json!({ "title": "t", "defaultRole": "HOST" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
}

/// 3. A meeting code does not imply host authority.
#[tokio::test]
async fn s03_code_is_not_authority() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        )
        .await;
    let code_str = space["meetingCode"].as_str().unwrap();
    let sid = uuid(&space["id"]);
    let (_, view) = app
        .get(
            &format!("/api/v2/meeting-spaces/{code_str}"),
            Auth::Bearer(&user.token),
        )
        .await;
    assert_eq!(view["view"], "SUMMARY");
    assert!(view.get("ownerUserId").is_none());
    let (_, c) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let cid = uuid(&c["id"]);
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/start"),
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    app.post(
        &format!("/api/v2/conferences/{cid}/start"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (_, j) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(j["participant"]["role"], "PARTICIPANT");
    let (s, v) = app
        .get(
            &format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Bearer(&user.token),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/lock"),
            Auth::None,
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// 4. A valid passcode does not make the actor a member or host.
#[tokio::test]
async fn s04_passcode_grants_no_privilege() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC_PASSCODE", "passcode": "letmein1" }),
        )
        .await;
    let (_, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "g", "passcode": "letmein1" }),
        )
        .await;
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    let token = v["participantToken"].as_str().unwrap();
    let (s, _) = app
        .get(
            &format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Participant(token),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_members WHERE meeting_space_id='{sid}'"
        ))
        .await,
        0
    );
    // Passcode attempts are rate limited (no Redis needed) and audited without the value.
    for _ in 0..30 {
        app.join(
            cid,
            Auth::None,
            json!({ "displayName": "g", "passcode": "wrong" }),
        )
        .await;
    }
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "g", "passcode": "wrong" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::TOO_MANY_REQUESTS, "RATE_LIMITED")
    );
    assert!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{sid}' AND action='PASSCODE_FAILED'")).await > 0);
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM conference_audit_events WHERE metadata::text LIKE '%wrong%'"
        )
        .await,
        0
    );
}

/// 5. An invitation cannot grant a role above the inviter's authority.
#[tokio::test]
async fn s05_invitation_cannot_escalate() {
    let app = app!();
    let owner = app.user("owner").await;
    let cohost = app.user("cohost").await;
    let participant = app.user("participant").await;
    let space = app.create_space(&owner, json!({ "title": "x" })).await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner, &cohost, Role::CoHost).await;
    app.add_member(sid, &owner, &participant, Role::Participant)
        .await;
    let url = format!("/api/v2/meeting-spaces/{sid}/invitations");
    for role in ["OWNER", "HOST", "CO_HOST"] {
        let (s, v) = app
            .post(&url, Auth::Bearer(&cohost.token), json!({ "role": role }))
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
            "{role}"
        );
    }
    let (s, _) = app
        .post(
            &url,
            Auth::Bearer(&cohost.token),
            json!({ "role": "MODERATOR" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app
        .post(&url, Auth::Bearer(&participant.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .post(&url, Auth::Bearer(&owner.token), json!({ "role": "OWNER" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "ownership never via invitation");
}

/// 6/7/8. Expired, revoked and exhausted invitations fail (also at resolve).
#[tokio::test]
async fn s06_07_08_invitation_lifecycle() {
    let app = app!();
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let url = format!("/api/v2/meeting-spaces/{sid}/invitations");
    let (_, exp) = app.post(&url, Auth::Bearer(&owner.token), json!({})).await;
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE meeting_invitations SET created_at = now() - interval '2 days', expires_at = now() - interval '1 second' WHERE id='{}'",
            exp["invitation"]["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
    let (s, v) = app
        .post(
            &format!(
                "/api/v2/invitations/{}/resolve",
                exp["token"].as_str().unwrap()
            ),
            Auth::None,
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_EXPIRED"));
    let (_, rev) = app.post(&url, Auth::Bearer(&owner.token), json!({})).await;
    app.post(
        &format!("{url}/{}/revoke", rev["invitation"]["id"].as_str().unwrap()),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "x", "invitationToken": rev["token"] }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_INVALID"));
    let (_, one) = app
        .post(&url, Auth::Bearer(&owner.token), json!({ "useLimit": 1 }))
        .await;
    let (s, _) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "a", "invitationToken": one["token"] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "b", "invitationToken": one["token"] }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_INVALID"));
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "c", "invitationToken": "inv_forged" }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_INVALID"));
    // Invitation of another space is not valid here.
    let other = app.create_space(&owner, json!({ "title": "other" })).await;
    let (_, foreign) = app
        .post(
            &format!(
                "/api/v2/meeting-spaces/{}/invitations",
                other["id"].as_str().unwrap()
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "d", "invitationToken": foreign["token"] }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::GONE, "INVITATION_INVALID"));
}

/// 9. Copying a guest's display name cannot impersonate that guest.
#[tokio::test]
async fn s09_display_name_is_not_identity() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let (a, token_a) = guest(&app, cid, "Dr Smith").await;
    app.post(
        &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
        Auth::Bearer(&owner.token),
        json!({ "participantIds": [a] }),
    )
    .await;
    let (b, token_b) = guest(&app, cid, "Dr Smith").await;
    assert_ne!(a, b);
    assert_ne!(token_a, token_b);
    let (_, me_b) = app
        .get(
            &format!("/api/v2/conferences/{cid}/participants/me"),
            Auth::Participant(&token_b),
        )
        .await;
    assert_eq!(uuid(&me_b["id"]), b);
    assert_eq!(
        me_b["admissionState"], "WAITING",
        "copycat does not inherit admission"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(DISTINCT guest_identity) FROM conference_participants WHERE conference_id='{cid}' AND display_name='Dr Smith'")).await,
        2
    );
}

/// 10. A guest cannot reuse another guest's identity/session.
#[tokio::test]
async fn s10_guest_credentials_are_scoped() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, conf_a) = app
        .live_conference(&owner, json!({ "title": "a", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, conf_b) = app
        .live_conference(&owner, json!({ "title": "b", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, token_a) = guest(&app, conf_a, "alice").await;
    let (s, v) = app
        .join(conf_b, Auth::Participant(&token_a), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED"),
        "credential bound to its conference"
    );
    let (s, _) = app
        .join(conf_a, Auth::Participant("pt_guessed"), json!({}))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, bob) = app
        .join(conf_a, Auth::None, json!({ "displayName": "bob" }))
        .await;
    let bob_session = bob["participantSession"]["id"].clone();
    let (s, v) = app
        .join(
            conf_a,
            Auth::Participant(&token_a),
            json!({ "previousSessionId": bob_session }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        "cannot hijack another participant's session"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE id='{}' AND state='PENDING'",
            bob_session.as_str().unwrap()
        ))
        .await,
        1
    );
    // Credentials are stored hashed only.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE credential_hash = '{token_a}'"
        ))
        .await,
        0
    );
}

async fn cross_setup(app: &TestApp) -> (User, Uuid, Uuid, Uuid) {
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
    let (victim, _) = guest(app, conf_b, "victim").await;
    (host_a, conf_a, conf_b, victim)
}

async fn victim_untouched(app: &TestApp, victim: Uuid) {
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{victim}' AND admission_state='WAITING' AND role='PARTICIPANT' AND version=1")).await,
        1
    );
    assert_eq!(
        app.rtc
            .other_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no provider interaction"
    );
}

/// 11/12/13/14. Host A cannot admit / deny / role-change / remove / ban / return-to-waiting
/// a participant of conference B.
#[tokio::test]
async fn s11_to_14_cross_conference_idor() {
    let app = app!();
    let (host_a, conf_a, _conf_b, victim) = cross_setup(&app).await;
    let t = Auth::Bearer(&host_a.token);
    let base = format!("/api/v2/conferences/{conf_a}");
    let checks = vec![
        app.post(
            &format!("{base}/waiting-room/admit"),
            t,
            json!({ "participantIds": [victim] }),
        )
        .await,
        app.post(
            &format!("{base}/waiting-room/deny"),
            t,
            json!({ "participantIds": [victim] }),
        )
        .await,
        app.patch(
            &format!("{base}/participants/{victim}/role"),
            t,
            json!({ "role": "VIEWER" }),
        )
        .await,
        app.post(
            &format!("{base}/participants/{victim}/remove"),
            t,
            json!({}),
        )
        .await,
        app.post(&format!("{base}/participants/{victim}/ban"), t, json!({}))
            .await,
        app.post(
            &format!("{base}/participants/{victim}/return-to-waiting"),
            t,
            json!({}),
        )
        .await,
    ];
    for (s, v) in checks {
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, "PARTICIPANT_NOT_FOUND"),
            "{v}"
        );
    }
    victim_untouched(&app, victim).await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_bans WHERE participant_id='{victim}'"
        ))
        .await,
        0
    );
    // Mixed bulk request: valid own id + foreign id → whole request rejected, nothing changes.
    let (own, _) = guest(&app, conf_a, "own").await;
    let (s, _) = app
        .post(
            &format!("{base}/waiting-room/admit"),
            t,
            json!({ "participantIds": [own, victim] }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{own}' AND admission_state='WAITING'")).await, 1);
}

/// 15. A member cannot self-promote to host/co-host.
#[tokio::test]
async fn s15_member_cannot_self_promote() {
    let app = app!();
    let owner = app.user("owner").await;
    let host = app.user("host").await;
    let member = app.user("member").await;
    let space = app.create_space(&owner, json!({ "title": "x" })).await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner, &host, Role::Host).await;
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let find = |u: Uuid| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| uuid(&m["userId"]) == u)
            .unwrap()
            .clone()
    };
    let m = find(member.id);
    let h = find(host.id);
    let (s, _) = app
        .patch(
            &format!(
                "/api/v2/meeting-spaces/{sid}/members/{}",
                m["id"].as_str().unwrap()
            ),
            Auth::Bearer(&member.token),
            json!({ "role": "HOST", "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .patch(
            &format!(
                "/api/v2/meeting-spaces/{sid}/members/{}",
                h["id"].as_str().unwrap()
            ),
            Auth::Bearer(&host.token),
            json!({ "role": "CO_HOST", "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "cannot modify own membership");
    let (s, _) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&host.token),
            json!({ "userId": member.id, "role": "HOST" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "host cannot mint peer hosts");
    let (_, j) = app.live_conference(&owner, json!({ "title": "c" })).await;
    let _ = j;
    // Live self-promotion as participant is covered in E2E 13.
}

/// 16. A banned participant cannot establish a new valid session (API and DB invariant).
#[tokio::test]
async fn s16_banned_cannot_hold_session() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    let (pid, token) = guest(&app, cid, "g").await;
    app.post(
        &format!("/api/v2/conferences/{cid}/participants/{pid}/ban"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
    let err = app
        .db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO participant_sessions (id, conference_participant_id, conference_id, media_identity, state, established_at) \
             VALUES (gen_random_uuid(), '{pid}', '{cid}', 'forced', 'PENDING', now())"
        ))
        .await;
    assert!(
        err.is_err(),
        "DB trigger forbids active sessions for non-admitted participants"
    );
}

/// 17. A removed participant cannot regain authority through stale client state.
#[tokio::test]
async fn s17_removed_has_no_authority() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        )
        .await;
    let (pid, token) = guest(&app, cid, "helper").await;
    app.post(
        &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
        Auth::Bearer(&owner.token),
        json!({ "participantIds": [pid] }),
    )
    .await;
    let (s, _) = app
        .patch(
            &format!("/api/v2/conferences/{cid}/participants/{pid}/role"),
            Auth::Bearer(&owner.token),
            json!({ "role": "CO_HOST" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = app
        .get(
            &format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Participant(&token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "promoted guest co-host has authority");
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/participants/{pid}/remove"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = app
        .get(
            &format!("/api/v2/conferences/{cid}/waiting-room"),
            Auth::Participant(&token),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{cid}/lock"),
            Auth::Participant(&token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app.join(cid, Auth::Participant(&token), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
}

/// 18. An ENDING conference cannot admit.
#[tokio::test]
async fn s18_ending_cannot_admit() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    let (pid, _) = guest(&app, cid, "w").await;
    app.post(
        &format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app
        .post(
            &format!("/api/v2/conferences/{cid}/waiting-room/admit"),
            Auth::Bearer(&owner.token),
            json!({ "participantIds": [pid] }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{pid}' AND admission_state='WAITING'")).await, 1);
}

/// 19. ENDED (and CANCELLED) conferences cannot be joined or reconnected.
#[tokio::test]
async fn s19_ended_cannot_join() {
    let app = app!();
    let owner = app.user("owner").await;
    let user = app.user("user").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "x", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let (s, _) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!(s, StatusCode::OK);
    app.post(
        &format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    // Simulate the Part E provider confirming termination.
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE conference_sessions SET status='ENDED', ended_at=now() WHERE id='{cid}'"
        ))
        .await
        .unwrap();
    let (s, v) = app.join(cid, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    let (_, c2) = app
        .post(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let cid2 = uuid(&c2["id"]);
    app.post(
        &format!("/api/v2/conferences/{cid2}/cancel"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app.join(cid2, Auth::Bearer(&user.token), json!({})).await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
}

/// 20. Organization / domain / healthcare / invite-only access modes reject unauthorized actors.
#[tokio::test]
async fn s20_private_access_modes() {
    let org = Uuid::new_v4();
    let mut opts = TestOptions::default();
    // Users are created after the app; register directory entries via fixed ids below.
    let owner_id = Uuid::new_v4();
    let insider_id = Uuid::new_v4();
    opts.organizations = HashMap::from([(owner_id, vec![org]), (insider_id, vec![org])]);
    let app = app!(opts);
    let mk = |id: Uuid, name: &str, email: &str| {
        let app = &app;
        let name = name.to_string();
        let email = email.to_string();
        async move {
            app.db
                .conn
                .execute_unprepared(&format!(
                    "INSERT INTO users (id, username, email, auth_type, role, status) VALUES ('{id}', '{name}', '{email}', 'local', 'user', 'active')"
                ))
                .await
                .unwrap();
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
    };
    let owner = mk(owner_id, "org-owner", "owner@acme.io").await;
    let insider = mk(insider_id, "insider", "insider@acme.io").await;
    let outsider = app.user_with_email("outsider", "outsider@evil.io").await;
    let (_, org_conf) = app
        .live_conference(
            &owner,
            json!({ "title": "o", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": org }),
        )
        .await;
    assert_eq!(
        app.join(org_conf, Auth::Bearer(&insider.token), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let (s, v) = app
        .join(org_conf, Auth::Bearer(&outsider.token), json!({}))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "MEETING_ACCESS_DENIED")
    );
    assert_eq!(
        app.join(org_conf, Auth::None, json!({ "displayName": "g" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let (_, dom_conf) = app
        .live_conference(&owner, json!({ "title": "d", "accessPolicy": "DOMAIN_RESTRICTED", "accessConfig": { "allowedDomains": ["acme.io"] } }))
        .await;
    assert_eq!(
        app.join(dom_conf, Auth::Bearer(&insider.token), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app.join(dom_conf, Auth::Bearer(&outsider.token), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    let (_, auth_conf) = app
        .live_conference(
            &owner,
            json!({ "title": "a", "accessPolicy": "AUTHENTICATED" }),
        )
        .await;
    let (s, v) = app
        .join(auth_conf, Auth::None, json!({ "displayName": "g" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
}

/// 20b. Healthcare appointment access goes through the server-side adapter only.
#[tokio::test]
async fn s20b_healthcare_adapter() {
    let patient_id = Uuid::new_v4();
    let opts = TestOptions {
        healthcare_allow: vec![patient_id],
        ..TestOptions::default()
    };
    let app = app!(opts);
    let clinician = app.user("clinician").await;
    app.db
        .conn
        .execute_unprepared(&format!("INSERT INTO users (id, username, email, auth_type, role, status) VALUES ('{patient_id}', 'patient', 'p@x.io', 'local', 'user', 'active')"))
        .await
        .unwrap();
    let patient_token = app
        .state
        .jwt_service
        .generate_access_token(patient_id, "user")
        .unwrap();
    let stranger = app.user("stranger").await;
    let (_, cid) = app
        .live_conference(
            &clinician,
            json!({ "title": "Consult", "meetingType": "HEALTHCARE", "securityProfile": "HEALTHCARE", "accessPolicy": "HEALTHCARE_APPOINTMENT", "accessConfig": { "adapterRef": "healthnet-appointments" } }),
        )
        .await;
    assert_eq!(
        app.join(cid, Auth::Bearer(&patient_token), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app.join(cid, Auth::Bearer(&stranger.token), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Healthcare spaces hide their title from non-managers.
    let (_, sp) = app
        .get(
            &format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&stranger.token),
        )
        .await;
    assert_eq!(sp["view"], "SUMMARY");
}

/// 21. The request body cannot override organization identity.
#[tokio::test]
async fn s21_body_cannot_claim_organization() {
    let app = app!();
    let user = app.user("user").await;
    let foreign_org = Uuid::new_v4();
    let (s, v) = app
        .post("/api/v2/meeting-spaces", Auth::Bearer(&user.token), json!({ "title": "x", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": foreign_org }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "x", "accessPolicy": "PUBLIC" }))
        .await;
    let (s, v) = app
        .join(
            cid,
            Auth::Bearer(&user.token),
            json!({ "organizationId": foreign_org }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
}

/// 22. A stale policy update cannot silently overwrite a newer version.
#[tokio::test]
async fn s22_stale_update_rejected() {
    let app = app!();
    let owner = app.user("owner").await;
    let stranger = app.user("stranger").await;
    let space = app.create_space(&owner, json!({ "title": "x" })).await;
    let sid = uuid(&space["id"]);
    let url = format!("/api/v2/meeting-spaces/{sid}");
    let (s, v) = app
        .patch(
            &url,
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "accessPolicy": "PUBLIC" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["version"], 2);
    let (s, v) = app
        .patch(
            &url,
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "MEETING_VERSION_CONFLICT")
    );
    let (_, now) = app.get(&url, Auth::Bearer(&owner.token)).await;
    assert_eq!(now["accessPolicy"], "PUBLIC");
    let (s, _) = app
        .patch(
            &url,
            Auth::Bearer(&stranger.token),
            json!({ "version": 2, "title": "hijack" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Ownership never changes through PATCH.
    let (s, _) = app
        .patch(
            &url,
            Auth::Bearer(&owner.token),
            json!({ "version": 2, "ownerUserId": stranger.id }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{sid}' AND action='SPACE_POLICY_UPDATE'")).await, 1);
}

/// Error envelope: stable code + requestId, never SQL/internal details.
#[tokio::test]
async fn s23_error_envelope_is_safe() {
    let app = app!();
    let (s, v) = app
        .get(
            &format!("/api/v2/conferences/{}", Uuid::new_v4()),
            Auth::None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(code(&v), "CONFERENCE_NOT_FOUND");
    assert!(v["error"]["requestId"]
        .as_str()
        .unwrap()
        .starts_with("req_"));
    let (s, v) = app.get("/api/v2/conferences/not-a-uuid", Auth::None).await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    let text = v.to_string().to_lowercase();
    assert!(!text.contains("sql") && !text.contains("sea_orm") && !text.contains("panicked"));
}
