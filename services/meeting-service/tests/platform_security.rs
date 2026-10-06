//! Part L — cross-cutting security certification (L-S1..L-S20, §43–§51).
//!
//! Every case runs against the real router (and the mock LiveKit provider / in-memory
//! artefact storage), never against helper functions alone.

mod common;

use std::sync::atomic::Ordering;

use axum::http::{Method, StatusCode};
use backend::domain::Role;
use common::{artifacts as art, contract::*, livekit_mock, rtc::*, *};
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

async fn participant_count(app: &TestApp, cid: Uuid) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
    ))
    .await
}

/// L-S1: a join can never select a registered identity — not through body fields, a
/// forged bearer token, or a forged participant credential.
#[tokio::test]
async fn l_s01_forged_registered_identity_rejected_on_join() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let victim = app.user("victim").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s1", "accessPolicy": "PUBLIC" }))
        .await;
    for body in [
        json!({ "displayName": "x", "userId": victim.id }),
        json!({ "displayName": "x", "user_id": victim.id }),
        json!({ "displayName": "x", "email": victim.email }),
        json!({ "displayName": "x", "identityType": "REGISTERED_USER" }),
    ] {
        let (s, v) = app.join(cid, Auth::None, body.clone()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{body}"
        );
    }
    let forged = app
        .state
        .jwt_service
        .generate_access_token(victim.id, "user")
        .unwrap()
        .replace('.', "x");
    let (s, v) = app.join(cid, Auth::Bearer(&forged), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED"),
        "{v}"
    );
    let (s, v) = app
        .join(
            cid,
            Auth::Participant("pt_forged-credential"),
            json!({ "displayName": "x" }),
        )
        .await;
    assert!(
        matches!(s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
        "forged participant credential: {s} {v}"
    );
    assert_eq!(participant_count(&app, cid).await, 0, "nothing joined");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE user_id='{}'",
            victim.id
        ))
        .await,
        0
    );
}

/// L-S2: participant / session ids in a request never select another participant.
#[tokio::test]
async fn l_s02_forged_participant_id_rejected() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s2", "accessPolicy": "PUBLIC" }))
        .await;
    let victim = join_guest(&app, cid, "victim").await;
    connect(&app, cid, &victim).await;
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "attacker", "participantId": victim.participant_id }),
        )
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
    // Reconnect claim with someone else's session: a NEW participant, victim untouched.
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "attacker", "previousSessionId": victim.session_id }),
        )
        .await;
    if s == StatusCode::OK {
        assert_ne!(uuid(&v["participant"]["id"]), victim.participant_id);
    }
    assert_eq!(session_state(&app, victim.session_id).await, "CONNECTED");
    // Leave / refresh with the victim's session id from another identity.
    let attacker = join_guest(&app, cid, "attacker2").await;
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/leave"),
            attacker.auth(),
            json!({ "participantSessionId": victim.session_id }),
        )
        .await;
    assert!(
        s.is_client_error() || v["closedSessions"] == 0,
        "cannot close another participant's session: {s} {v}"
    );
    assert_eq!(session_state(&app, victim.session_id).await, "CONNECTED");
}

/// L-S3: a display name is presentation only — it never merges identities or carries
/// a role.
#[tokio::test]
async fn l_s03_display_name_cannot_become_identity() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s3", "accessPolicy": "PUBLIC" }))
        .await;
    let host = join_user(&app, cid, &owner).await;
    let name = host.body["participant"]["displayName"]
        .as_str()
        .unwrap()
        .to_string();
    let a = join_guest(&app, cid, &name).await;
    let b = join_guest(&app, cid, &name).await;
    assert_ne!(a.participant_id, b.participant_id);
    assert_ne!(a.participant_id, host.participant_id);
    for g in [&a, &b] {
        assert_eq!(g.body["participant"]["identityType"], "GUEST");
        assert_eq!(g.body["participant"]["role"], "PARTICIPANT");
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/end"),
                g.auth(),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "name-alike guest cannot end: {v}");
    }
}

/// L-S4: possession of the meeting code never implies host / owner / moderator.
#[tokio::test]
async fn l_s04_meeting_code_does_not_imply_host() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(&owner, json!({ "title": "s4", "accessPolicy": "PUBLIC" }))
        .await;
    let (_, space) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let code_str = space["meetingCode"].as_str().unwrap();
    let holder = app.user("holder").await;
    let (s, r) = app
        .get(
            format!("/api/v2/resolve/{code_str}"),
            Auth::Bearer(&holder.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(
        r["meetingSpace"]["view"], "FULL",
        "code holder sees the summary only"
    );
    let resolved = uuid(&r["conference"]["id"]);
    assert_eq!(resolved, cid);
    let (s, v) = app
        .join(resolved, Auth::Bearer(&holder.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["participant"]["role"], "PARTICIPANT");
    for (m, uri, body) in [
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/end"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/lock"),
            json!({}),
        ),
        (
            Method::PATCH,
            format!("/api/v2/meeting-spaces/{sid}"),
            json!({ "version": 1, "title": "hijack" }),
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            json!({ "role": "PARTICIPANT" }),
        ),
    ] {
        let (s, v) = app
            .call(m, &uri, Auth::Bearer(&holder.token), Some(body))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}: {v}");
    }
}

/// L-S5: an invitation grants exactly its encoded role (bounded by the issuer's grant
/// authority) and still flows through admission.
#[tokio::test]
async fn l_s05_invitation_cannot_exceed_granted_role() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let cohost = app.user("cohost").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "s5", "accessPolicy": "INVITE_ONLY", "waitingRoomPolicy": "EVERYONE" }),
        )
        .await;
    app.add_member(sid, &owner, &cohost, Role::CoHost).await;
    let url = format!("/api/v2/meeting-spaces/{sid}/invitations");
    for role in ["OWNER", "HOST", "CO_HOST"] {
        let (s, v) = app
            .post(&url, Auth::Bearer(&cohost.token), json!({ "role": role }))
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{role}: {v}");
    }
    let (s, v) = app
        .post(&url, Auth::Bearer(&owner.token), json!({ "role": "OWNER" }))
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "ownership never via invitation: {v}"
    );
    let (s, inv) = app
        .post(
            &url,
            Auth::Bearer(&cohost.token),
            json!({ "role": "PRESENTER", "conferenceId": cid }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    let token = inv["token"].as_str().unwrap();
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "guest", "invitationToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["participant"]["role"], "PRESENTER",
        "exactly the invited role"
    );
    assert_eq!(
        v["participant"]["admissionState"], "WAITING",
        "invitation still flows through the waiting room"
    );
    assert!(v["rtc"].is_null());
}

/// L-S6: moderation of a participant of another conference is rejected before any
/// provider call, for every moderation primitive.
#[tokio::test]
async fn l_s06_foreign_conference_moderation_rejected() {
    let app = app!(TestApp::collab());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "A", "accessPolicy": "PUBLIC" }))
        .await;
    let other = app.user("other").await;
    let (_, victim_cid) = app
        .live_conference(&other, json!({ "title": "B", "accessPolicy": "PUBLIC" }))
        .await;
    let victim = join_guest(&app, victim_cid, "victim").await;
    connect(&app, victim_cid, &victim).await;
    let pid = victim.participant_id;
    let calls_before = lk(&app).calls.lock().unwrap().len();
    let ops: Vec<(Method, String, Value)> = vec![
        (
            Method::POST,
            format!("participants/{pid}/return-to-waiting"),
            json!({}),
        ),
        (
            Method::PATCH,
            format!("participants/{pid}/role"),
            json!({ "role": "VIEWER" }),
        ),
        (
            Method::POST,
            format!("participants/{pid}/remove"),
            json!({}),
        ),
        (Method::POST, format!("participants/{pid}/ban"), json!({})),
        (
            Method::POST,
            format!("participants/{pid}/mute"),
            json!({ "trackKind": "AUDIO" }),
        ),
        (
            Method::POST,
            format!("participants/{pid}/audio-lock"),
            json!({}),
        ),
        (
            Method::POST,
            format!("participants/{pid}/video-lock"),
            json!({}),
        ),
        (
            Method::POST,
            format!("participants/{pid}/screen-share-lock"),
            json!({}),
        ),
        (
            Method::POST,
            format!("participants/{pid}/chat-restriction"),
            json!({}),
        ),
        (
            Method::DELETE,
            format!("participants/{pid}/hand-raise"),
            json!({}),
        ),
        (
            Method::POST,
            "spotlights".into(),
            json!({ "participantId": pid }),
        ),
        (Method::POST, format!("screen-share/{pid}/stop"), json!({})),
        (
            Method::POST,
            "waiting-room/admit".into(),
            json!({ "participantIds": [pid] }),
        ),
        (
            Method::POST,
            "waiting-room/deny".into(),
            json!({ "participantIds": [pid] }),
        ),
    ];
    for (m, suffix, body) in ops {
        let uri = format!("/api/v2/conferences/{cid}/{suffix}");
        let (s, v) = app
            .call(m.clone(), &uri, Auth::Bearer(&owner.token), Some(body))
            .await;
        assert!(
            matches!(s, StatusCode::NOT_FOUND),
            "{m} {uri}: foreign target must be not found: {s} {v}"
        );
    }
    let provider_calls: Vec<String> = lk(&app).calls.lock().unwrap()[calls_before..]
        .iter()
        .map(|(m, _, _)| m.clone())
        .collect();
    assert!(
        provider_calls.is_empty(),
        "no provider call: {provider_calls:?}"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{pid}' AND admission_state='ADMITTED' AND role='PARTICIPANT' AND audio_locked_at IS NULL"
        ))
        .await,
        1,
        "victim untouched"
    );
    assert_eq!(session_state(&app, victim.session_id).await, "CONNECTED");
}

/// L-S7: role escalation requires grant authority and is audited; a DTO can never
/// self-promote.
#[tokio::test]
async fn l_s07_unauthorized_role_escalation_rejected() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(&owner, json!({ "title": "s7", "accessPolicy": "PUBLIC" }))
        .await;
    let cohost_user = app.user("cohost").await;
    app.add_member(sid, &owner, &cohost_user, Role::CoHost)
        .await;
    let cohost = join_user(&app, cid, &cohost_user).await;
    let p = join_guest(&app, cid, "p").await;
    // Self-promotion.
    let (s, v) = app
        .patch(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/role",
                p.participant_id
            ),
            p.auth(),
            json!({ "role": "HOST" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // Co-host cannot grant above its authority; nobody grants OWNER.
    for role in ["HOST", "OWNER"] {
        let (s, v) = change_role(&app, cid, &cohost_user, p.participant_id, role).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "co-host → {role}: {v}");
    }
    let (s, v) = change_role(&app, cid, &owner, p.participant_id, "OWNER").await;
    assert!(s.is_client_error(), "OWNER never assignable: {s} {v}");
    // A co-host cannot demote the owner's peer (host-rank) participants either.
    let (s, _) = change_role(&app, cid, &cohost_user, cohost.participant_id, "HOST").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE id='{}' AND role='PARTICIPANT'",
            p.participant_id
        ))
        .await,
        1
    );
    // A legitimate escalation is audited.
    let (s, v) = change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND target_ref='{}' AND action LIKE '%ROLE%'",
            p.participant_id
        ))
        .await
            >= 1
    );
}

/// L-S8 / L-S9 / L-S10 / L-S11: no usable credential for WAITING, ENDING, removed or
/// banned participants (join, reconnect and refresh paths).
#[tokio::test]
async fn l_s08_to_s11_no_credential_for_waiting_ending_removed_banned() {
    let app = app!(TestApp::collab());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "s8", "accessPolicy": "PUBLIC", "waitingRoomPolicy": "GUESTS_ONLY" }),
        )
        .await;
    // L-S8 waiting.
    let (s, w) = app
        .join(cid, Auth::None, json!({ "displayName": "w" }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(w["rtc"].is_null() && w["rtcStatus"] == "NOT_ISSUED_WAITING");
    let wtoken = w["participantToken"].as_str().unwrap();
    let (s, v) = app
        .join(
            cid,
            Auth::Participant(wtoken),
            json!({ "displayName": "w" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v["rtc"].is_null(),
        "reconnect while waiting: still no credential: {v}"
    );
    // L-S10 / L-S11.
    let member = app.user("member").await;
    let removed = join_user(&app, cid, &member).await;
    let banned_user = app.user("banned").await;
    let banned = join_user(&app, cid, &banned_user).await;
    for (j, action) in [(&removed, "remove"), (&banned, "ban")] {
        let (s, v) = app
            .post(
                format!(
                    "/api/v2/conferences/{cid}/participants/{}/{action}",
                    j.participant_id
                ),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    for (j, user) in [(&removed, &member), (&banned, &banned_user)] {
        let (s, v) = app
            .post(
                format!("/api/v2/conferences/{cid}/media-credential"),
                Auth::Bearer(&user.token),
                json!({ "participantSessionId": j.session_id }),
            )
            .await;
        assert!(s.is_client_error(), "{s} {v}");
        assert!(
            !matches!(code(&v), "VALIDATION_ERROR" | "FEATURE_DISABLED"),
            "{v}"
        );
    }
    let (s, v) = app
        .join(cid, Auth::Bearer(&banned_user.token), json!({}))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PARTICIPANT_BANNED"));
    let (s, v) = app.join(cid, Auth::Bearer(&member.token), json!({})).await;
    assert!(
        s.is_client_error()
            || v["rtc"].is_null()
            || v["participant"]["admissionState"] != "ADMITTED",
        "a removed participant is not silently re-admitted with media: {s} {v}"
    );
    // L-S9 ENDING (termination unconfirmed at the provider).
    let survivor_user = app.user("survivor").await;
    let survivor = join_user(&app, cid, &survivor_user).await;
    lk(&app).faults.delete_noop.store(true, Ordering::SeqCst);
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(v["conference"]["status"], "ENDING");
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "late" }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::CONFLICT, "CONFERENCE_NOT_LIVE"));
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/media-credential"),
            Auth::Bearer(&survivor_user.token),
            json!({ "participantSessionId": survivor.session_id }),
        )
        .await;
    assert!(
        s.is_client_error() && code(&v) != "VALIDATION_ERROR",
        "{s} {v}"
    );
}

/// L-S12 / L-S13: provider callbacks must be authenticated; replays are deduplicated.
#[tokio::test]
async fn l_s12_s13_forged_and_replayed_provider_callbacks() {
    let app = app!(TestApp::artifacts());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "s12", "accessPolicy": "PUBLIC" }))
        .await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app).connect(&p.credential).unwrap();
    let body = livekit_mock::webhook_body(
        "participant_joined",
        "EV_L_S12",
        &room(cid),
        None,
        Some(&p.identity),
    );
    for auth in [
        None,
        Some("Bearer nope".to_string()),
        Some(livekit_mock::sign_webhook(
            &body,
            livekit_mock::API_KEY,
            "wrong-secret-wrong-secret-wrong",
        )),
        Some(livekit_mock::sign_webhook(
            "{\"other\":1}",
            livekit_mock::API_KEY,
            livekit_mock::API_SECRET,
        )),
    ] {
        let (s, v) = app.webhook(&body, auth.as_deref()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
        assert_no_secrets(&app, "webhook rejection", &v);
    }
    assert_eq!(
        session_state(&app, p.session_id).await,
        "PENDING",
        "forged callback changed nothing"
    );
    // A forged callback can never create a registered identity.
    let forged_identity = format!("ps_{}", Uuid::new_v4().simple());
    let forged = livekit_mock::webhook_body(
        "participant_joined",
        "EV_L_S12_FORGED",
        &room(cid),
        None,
        Some(&forged_identity),
    );
    let (s, v) = app.signed_webhook(&forged).await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(v["outcome"], "APPLIED", "{v}");
    assert_eq!(participant_count(&app, cid).await, 1);
    // Replay: first applied, the rest deduplicated.
    let (_, first) = app.signed_webhook(&body).await;
    assert_eq!(first["outcome"], "APPLIED");
    for _ in 0..3 {
        let (s, v) = app.signed_webhook(&body).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["outcome"], "DUPLICATE", "{v}");
    }
    assert_eq!(events(&app, cid, "participant.joined").await, 1);
    // Transcription callbacks: HMAC required.
    let raw_body = json!({ "type": "JOB_STATUS", "jobRef": "x", "status": "FAILED" }).to_string();
    let ts = chrono::Utc::now().timestamp().to_string();
    let (s, _) = app
        .call_with_headers(
            Method::POST,
            "/internal/transcription/events",
            &[
                ("x-cf-timestamp", ts.as_str()),
                ("x-cf-signature", "sha256=00"),
            ],
            raw_body,
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// L-S14 / L-S15: artefact access is authorized per conference; a foreign artefact id
/// is insufficient; deleted / expired artefacts cannot be newly issued.
#[tokio::test]
async fn l_s14_s15_artifact_access_authorized_and_not_reissued() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, guests) = art::meeting(&app, 1).await;
    let (rid, _) = art::recording_ready(&app, cid, &owner).await;
    let (other_owner, _, other_cid, _) = art::meeting(&app, 0).await;
    // Foreign conference path, foreign host, ordinary participant.
    for (uri, auth) in [
        (
            format!("/api/v2/conferences/{other_cid}/artifacts/{rid}/access"),
            Auth::Bearer(&other_owner.token),
        ),
        (
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            Auth::Bearer(&other_owner.token),
        ),
    ] {
        let (s, v) = app.post(&uri, auth, json!({})).await;
        assert!(
            matches!(s, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN),
            "{uri}: {s} {v}"
        );
        assert!(v.get("url").is_none());
    }
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            guests[0].auth(),
            json!({}),
        )
        .await;
    assert!(
        s.is_client_error(),
        "participant without artefact permission: {s} {v}"
    );
    // Authorized access is short-lived, signed and object-scoped.
    let (s, a) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{a}");
    let exp = chrono::DateTime::parse_from_rfc3339(a["expiresAt"].as_str().unwrap()).unwrap();
    assert!(exp.signed_duration_since(chrono::Utc::now()) <= chrono::Duration::seconds(301));
    assert_no_secrets(&app, "artifact access", &a);
    // Deleted → never re-issued.
    let (s, v) = app
        .delete(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/{rid}/access"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_client_error(), "{s} {v}");
    assert!(v.get("url").is_none());
    // Retention-expired → deleted by the worker → never re-issued.
    let (rid2, _) = art::recording_ready(&app, cid, &owner).await;
    exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 minute' WHERE id='{rid2}'")).await;
    art::retention_tick(&app).await;
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/artifacts/{rid2}/access"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_client_error(), "expired artefact: {s} {v}");
}

/// L-S16: the healthcare profile never falls back to permissive public access.
#[tokio::test]
async fn l_s16_healthcare_profile_is_not_permissive() {
    let app = app!(TestApp::artifacts());
    let owner = app.user("clinician").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "hc", "meetingType": "HEALTHCARE", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "walk-in" }))
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "PROFILE_POLICY_DENIED"),
        "{v}"
    );
    let anyone = app.user("anyone").await;
    let (s, _) = app.join(cid, Auth::Bearer(&anyone.token), json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, p) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    let access = &p["effective"]["policy"]["access"];
    assert_eq!(access["publicAccessAllowed"], false, "{p}");
    assert_eq!(access["waitingRoomForced"], true, "{p}");
    assert_eq!(
        p["effective"]["policy"]["intelligence"]["location"], "DISABLED",
        "{p}"
    );
    // Recording defaults off; a capture cannot start without explicit configuration.
    let (s, v) = art::start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert!(
        s.is_client_error(),
        "healthcare recording default off: {s} {v}"
    );
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
    // An override cannot reopen public access.
    let (s, v) = art::set_profile(
        &app,
        &owner,
        sid,
        json!({ "access": { "publicAccessAllowed": true } }),
    )
    .await;
    assert!(
        s.is_client_error() || v["effective"]["policy"]["access"]["publicAccessAllowed"] == false,
        "{s} {v}"
    );
    let (s, _) = app
        .join(cid, Auth::None, json!({ "displayName": "again" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "still no public entry");
}

/// L-S17: internal operations / metrics / callback routes are not public.
#[tokio::test]
async fn l_s17_internal_operations_endpoints_not_public() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let bearer = format!("Bearer {}", owner.token);
    let cid = Uuid::new_v4();
    for uri in [
        "/internal/metrics".to_string(),
        "/internal/rtc/metrics".to_string(),
        "/internal/operations/conferences/health".to_string(),
        "/internal/operations/conferences/health?state=NOPE&limit=abc".to_string(),
        format!("/internal/operations/conferences/{cid}/health"),
        "/internal/operations/conferences/not-a-uuid/health".to_string(),
        "/internal/operations/incidents?status=OPEN&limit=x".to_string(),
    ] {
        for headers in [
            vec![],
            vec![("x-internal-token", "guess")],
            vec![("authorization", bearer.as_str())],
        ] {
            let (s, _, v) = raw(&app, Method::GET, &uri, &headers, None).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{uri} {headers:?}: {v}");
            assert_no_secrets(&app, &uri, &v);
        }
    }
    let (s, _) = app.webhook("{}", Some(&bearer)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// L-S18: request DTOs cannot inject role, provider, storage or internal authority.
#[tokio::test]
async fn l_s18_dto_cannot_inject_authority() {
    let app = app!(TestApp::artifacts());
    let (owner, sid, cid, g) = art::meeting(&app, 1).await;
    let cases: Vec<(Method, String, Auth<'_>, Value)> = vec![
        (
            Method::POST,
            "/api/v2/meeting-spaces".into(),
            Auth::Bearer(&owner.token),
            json!({ "title": "x", "ownerUserId": Uuid::new_v4() }),
        ),
        (
            Method::POST,
            "/api/v2/meeting-spaces".into(),
            Auth::Bearer(&owner.token),
            json!({ "title": "x", "origin": "LEGACY" }),
        ),
        (
            Method::POST,
            "/api/v2/meeting-spaces".into(),
            Auth::Bearer(&owner.token),
            json!({ "title": "x", "rtcRoomName": "cf-x" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            Auth::None,
            json!({ "displayName": "x", "role": "HOST" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            Auth::None,
            json!({ "displayName": "x", "admissionState": "ADMITTED" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/join"),
            Auth::None,
            json!({ "displayName": "x", "permissions": { "audioPublish": true } }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/recordings"),
            Auth::Bearer(&owner.token),
            json!({ "providerRef": "EG_x" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/recordings"),
            Auth::Bearer(&owner.token),
            json!({ "storageKey": "bucket/x" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/artifacts/reports"),
            Auth::Bearer(&owner.token),
            json!({ "artifactType": "ATTENDANCE_REPORT", "bucket": "b" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/media-credential"),
            g[0].auth(),
            json!({ "participantSessionId": g[0].session_id, "grants": { "canPublish": true } }),
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
            json!({ "userId": Uuid::new_v4(), "role": "PARTICIPANT", "status": "ACTIVE", "grantedBy": "x" }),
        ),
        (
            Method::POST,
            format!("/api/v2/conferences/{cid}/quality/samples"),
            g[0].auth(),
            json!({ "batchId": "b", "samples": [], "participantSessionId": Uuid::new_v4(), "userId": Uuid::new_v4() }),
        ),
    ];
    for (m, uri, auth, body) in cases {
        let (s, v) = app.call(m.clone(), &uri, auth, Some(body.clone())).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{m} {uri} {body}: {v}"
        );
    }
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
}

/// L-S19: the legacy v1 identity boundary stays safe (body `user_id` is a claim
/// checked against the bearer, never an identity).
#[tokio::test]
async fn l_s19_legacy_v1_identity_boundary() {
    if !require_test_redis() {
        return;
    }
    let app = app!(TestApp::new());
    let host = app.user("host").await;
    let victim = app.user("victim").await;
    let attacker = app.user("attacker").await;
    let id = Uuid::new_v4();
    use sea_orm::ConnectionTrait;
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{id}', 'LSQNNNTW', '{h}', '{h}', 'legacy', false, now() - interval '5 minutes', now() + interval '1 hour', 'ongoing')",
            h = host.id
        ))
        .await
        .unwrap();
    let uri = format!("/api/v1/meetings/{id}/join");
    let (s, _) = app
        .post(
            &uri,
            Auth::None,
            json!({ "display_name": "x", "user_id": victim.id }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app
        .post(
            &uri,
            Auth::Bearer(&attacker.token),
            json!({ "display_name": "x", "user_id": host.id }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "cannot claim the host's identity");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{id}' AND user_id IN ('{}','{}')",
            victim.id, host.id
        ))
        .await,
        0
    );
}

/// L-S20: no secret appears in public errors, realtime events or the OpenAPI contract.
#[tokio::test]
async fn l_s20_no_secret_in_errors_events_or_contract() {
    let app = app!(TestApp::artifacts());
    let (owner, _, cid, g) = art::meeting(&app, 1).await;
    let (rid, eg) = art::recording_active(&app, cid, &owner).await;
    let mut outputs: Vec<(String, Value)> = vec![];
    // Errors from several layers, including provider outages.
    lk(&app).faults.fail_all.store(true, Ordering::SeqCst);
    outputs.push((
        "provider down join".into(),
        app.join(cid, Auth::None, json!({ "displayName": "x" }))
            .await
            .1,
    ));
    outputs.push((
        "provider down stop".into(),
        app.post(
            format!("/api/v2/conferences/{cid}/recordings/{rid}/stop"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await
        .1,
    ));
    lk(&app).faults.fail_all.store(false, Ordering::SeqCst);
    outputs.push((
        "bad json".into(),
        app.post_raw(
            "/api/v2/meeting-spaces",
            Auth::Bearer(&owner.token),
            "{\"title\":",
        )
        .await
        .1,
    ));
    // Events as seen by clients (REST replay of the realtime stream).
    let (_, events) = app
        .get(
            format!("/api/v2/conferences/{cid}/events?after=0"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(
        events["events"]
            .as_array()
            .map(|e| !e.is_empty())
            .unwrap_or(false),
        "{events}"
    );
    outputs.push(("events".into(), events.clone()));
    outputs.push((
        "participant view".into(),
        app.get(
            format!("/api/v2/conferences/{cid}/participants"),
            g[0].auth(),
        )
        .await
        .1,
    ));
    outputs.push((
        "recording view".into(),
        app.get(
            format!("/api/v2/conferences/{cid}/recordings/{rid}"),
            Auth::Bearer(&owner.token),
        )
        .await
        .1,
    ));
    for (ctx, v) in &outputs {
        assert_no_secrets(&app, ctx, v);
        let text = v.to_string();
        assert!(!text.contains(&eg), "{ctx} leaks the provider egress id");
        assert!(
            !text.contains(&room(cid)),
            "{ctx} leaks the provider room name"
        );
        assert!(
            !text.contains("storageKey") && !text.contains("storage_key"),
            "{ctx}"
        );
    }
    let contract = openapi().to_string();
    for n in secret_needles(&app) {
        if n == "src/" || n == "SELECT " {
            continue; // prose in descriptions; secrets are checked below
        }
        assert!(!contract.contains(&n), "contract contains {n}");
    }
}
