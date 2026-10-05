//! Part M §26 — token / credential replay. Business state always prevails over a
//! stale provider or client credential: a credential minted before a removal, ban,
//! demotion, media lock, breakout move, conference end or session expiry can never
//! restore the authority it carried.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::{collab::*, livekit_mock, rtc::*, *};
use livekit_protocol::TrackSource;
use serde_json::json;
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

/// Provider join for `identity` in `room_name` (verified callback).
async fn joined_in(app: &TestApp, room_name: &str, identity: &str) -> serde_json::Value {
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        room_name,
        None,
        Some(identity),
    );
    let (s, v) = app.signed_webhook(&body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn refresh_body(j: &Joined) -> serde_json::Value {
    json!({ "participantSessionId": j.session_id })
}

/// Expired session (validity window passed while the row is still active): no new
/// credential, the provider join is evicted, the reconciler ends the session and
/// evicts the identity. Regression for the Part M expiry defect.
#[tokio::test]
async fn replay_expired_session() {
    let app = app!(TestApp::collab());
    let (_owner, cid, g) = meeting(&app, public(), 2).await;
    let (expired, other) = (&g[0], &g[1]);
    exec(&app, &format!("UPDATE participant_sessions SET expires_at = now() - interval '1 second' WHERE id='{}'", expired.session_id)).await;
    let (s, v) = refresh(&app, cid, expired, expired.auth()).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "PARTICIPANT_STATE_CONFLICT"),
        "{v}"
    );
    assert!(v.get("credential").is_none());
    // Re-connect with the old (still cryptographically valid) credential.
    lk(&app).disconnect(&room(cid), &expired.identity);
    if lk(&app).connect(&expired.credential).is_ok() {
        let v = joined_in(&app, &room(cid), &expired.identity).await;
        assert_eq!(v["outcome"], "STALE_SESSION_EVICTED", "{v}");
        assert!(lk(&app)
            .participant(&room(cid), &expired.identity)
            .is_none());
    }
    // The other participant is untouched; the reconciler sweeps the expired row.
    exec(&app, &format!("UPDATE participant_sessions SET expires_at = now() - interval '1 second' WHERE id='{}'", other.session_id)).await;
    let r = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone())
        .tick()
        .await
        .unwrap();
    assert!(r.kinds.contains(&"SESSION_EXPIRED".to_string()), "{r:?}");
    assert_eq!(session_state(&app, other.session_id).await, "EXPIRED");
    assert!(
        lk(&app).participant(&room(cid), &other.identity).is_none(),
        "evicted at the provider"
    );
    // A fresh join re-runs authorization and yields a new, valid session.
    let (s, v) = app
        .join(cid, other.auth(), json!({ "displayName": "again" }))
        .await;
    assert_eq!(
        (s, v["rtcStatus"].as_str()),
        (StatusCode::OK, Some("ISSUED")),
        "{v}"
    );
    assert_ne!(uuid(&v["participantSession"]["id"]), other.session_id);
}

/// A refreshed credential never outlives its session.
#[tokio::test]
async fn refreshed_credential_is_capped_by_session_expiry() {
    let app = app!(TestApp::collab());
    let (_owner, cid, g) = meeting(&app, public(), 1).await;
    exec(&app, &format!("UPDATE participant_sessions SET expires_at = now() + interval '90 seconds' WHERE id='{}'", g[0].session_id)).await;
    let (s, v) = refresh(&app, cid, &g[0], g[0].auth()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let exp = chrono::DateTime::parse_from_rfc3339(
        v["rtc"]["expiresAt"]
            .as_str()
            .or(v["expiresAt"].as_str())
            .unwrap(),
    )
    .unwrap();
    assert!(
        exp.signed_duration_since(chrono::Utc::now()) <= chrono::Duration::seconds(91),
        "credential capped at the session window: {v}"
    );
}

/// Removed and banned participants: old credentials are evicted, refresh and rejoin
/// refused.
#[tokio::test]
async fn replay_removed_and_banned_credentials() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    for (j, action) in [(&g[0], "remove"), (&g[1], "ban")] {
        let (s, _) = app
            .post(
                format!(
                    "/api/v2/conferences/{cid}/participants/{}/{action}",
                    j.participant_id
                ),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        assert!(lk(&app).participant(&room(cid), &j.identity).is_none());
        if lk(&app).connect(&j.credential).is_ok() {
            let v = joined_in(&app, &room(cid), &j.identity).await;
            assert_eq!(v["outcome"], "STALE_SESSION_EVICTED", "{action}: {v}");
            assert!(lk(&app).participant(&room(cid), &j.identity).is_none());
        }
        let (s, _) = app
            .post(
                format!("{}/media-credential", base(cid)),
                j.auth(),
                refresh_body(j),
            )
            .await;
        assert!(s.is_client_error());
        let (s, v) = app.join(cid, j.auth(), json!({ "displayName": "x" })).await;
        assert!(s.is_client_error(), "{action} rejoin: {s} {v}");
    }
}

/// Pre-role-change and pre-media-lock credentials: connecting with them re-asserts the
/// CURRENT grants, so the old publish rights are not restored.
#[tokio::test]
async fn replay_pre_demotion_and_pre_lock_credentials() {
    let app = app!(TestApp::collab());
    let (owner, cid, _) = meeting(&app, public(), 0).await;
    // Pre-demotion: a PRESENTER credential, then demotion to VIEWER before connecting.
    let p = join_guest(&app, cid, "presenter").await;
    let (s, _) = change_role(&app, cid, &owner, p.participant_id, "PRESENTER").await;
    assert_eq!(s, StatusCode::OK);
    let (s, fresh) = app
        .post(
            format!("{}/media-credential", base(cid)),
            p.auth(),
            refresh_body(&p),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{fresh}");
    let presenter_credential = fresh["rtc"]["credential"].as_str().unwrap().to_string();
    let (s, _) = change_role(&app, cid, &owner, p.participant_id, "VIEWER").await;
    assert_eq!(s, StatusCode::OK);
    let identity = lk(&app).connect(&presenter_credential).unwrap();
    let v = joined_in(&app, &room(cid), &identity).await;
    assert_eq!(v["outcome"], "APPLIED", "{v}");
    assert!(
        lk(&app)
            .publish(&room(cid), &identity, TrackSource::Camera)
            .is_err(),
        "VIEWER grants re-asserted on connect"
    );
    // Pre-lock: credential minted, then audio locked, then connect.
    let q = join_guest(&app, cid, "locked").await;
    let (s, _) = app
        .post(
            format!("{}/participants/{}/audio-lock", base(cid), q.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let identity = lk(&app).connect(&q.credential).unwrap();
    joined_in(&app, &room(cid), &identity).await;
    assert!(
        lk(&app)
            .publish(&room(cid), &identity, TrackSource::Microphone)
            .is_err(),
        "lock re-asserted"
    );
    assert!(lk(&app)
        .publish(&room(cid), &identity, TrackSource::Camera)
        .is_ok());
}

/// Pre-breakout-move credential: joining the old room is evicted (stale media context).
#[tokio::test]
async fn replay_pre_breakout_move_credential() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    for r in &rooms {
        open_room(&app, cid, &owner, *r).await;
    }
    let mv = |to: Uuid| {
        let app = &app;
        let owner = &owner;
        let pid = g[0].participant_id;
        async move {
            let (s, v) = app
                .post(
                    format!("{}/breakouts/move", base(cid)),
                    Auth::Bearer(&owner.token),
                    json!({ "participantId": pid, "toBreakoutId": to }),
                )
                .await;
            assert_eq!(s, StatusCode::OK, "{v}");
        }
    };
    mv(rooms[0]).await;
    let (_, a_cred) = app
        .post(
            format!("{}/media-credential", base(cid)),
            g[0].auth(),
            refresh_body(&g[0]),
        )
        .await;
    let a_credential = a_cred["rtc"]["credential"].as_str().unwrap().to_string();
    mv(rooms[1]).await;
    let identity = lk(&app).connect(&a_credential).unwrap();
    let v = joined_in(&app, &breakout_room(rooms[0]), &identity).await;
    assert_eq!(v["outcome"], "STALE_SESSION_EVICTED", "{v}");
    assert!(lk(&app)
        .participant(&breakout_room(rooms[0]), &identity)
        .is_none());
}

/// Pre-ENDING credential: after END the room is gone or the join is evicted.
#[tokio::test]
async fn replay_pre_end_credential() {
    let app = app!(TestApp::collab());
    let (owner, cid, _) = meeting(&app, public(), 0).await;
    let p = join_guest(&app, cid, "p").await;
    lk(&app)
        .faults
        .delete_noop
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/end"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, v["conference"]["status"].as_str()),
        (StatusCode::OK, Some("ENDING"))
    );
    // The provider still has the room (termination unconfirmed): the SFU accepts the
    // JWT, the verified callback evicts it.
    let identity = lk(&app).connect(&p.credential).unwrap();
    let v = joined_in(&app, &room(cid), &identity).await;
    assert_eq!(v["outcome"], "STALE_SESSION_EVICTED", "{v}");
    assert!(lk(&app).participant(&room(cid), &identity).is_none());
    lk(&app)
        .faults
        .delete_noop
        .store(false, std::sync::atomic::Ordering::SeqCst);
    app.post(
        format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    assert!(
        lk(&app).connect(&p.credential).is_err(),
        "room gone after confirmed END"
    );
}

/// Revoked invitation (new use and reconnect of a participant admitted through it),
/// consumed one-use registration token, exhausted invitation.
#[tokio::test]
async fn replay_revoked_invitation_and_consumed_tokens() {
    let app = app!(TestApp::artifacts());
    let owner = app.user("owner").await;
    let (sid, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "inv", "accessPolicy": "INVITE_ONLY" }),
        )
        .await;
    let (_, inv) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "conferenceId": cid, "useLimit": 1 }),
        )
        .await;
    let token = inv["token"].as_str().unwrap().to_string();
    let inv_id = uuid(&inv["invitation"]["id"]);
    let (s, v) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "invitee", "invitationToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pt = v["participantToken"].as_str().unwrap().to_string();
    // Exhausted (use limit 1).
    let (s, _) = app
        .join(
            cid,
            Auth::None,
            json!({ "displayName": "someone-else", "invitationToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::GONE, "exhausted invitation");
    // Revoked: the admitted holder's reconnect is refused too.
    app.post(
        format!("/api/v2/meeting-spaces/{sid}/invitations/{inv_id}/revoke"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app.join(cid, Auth::Participant(&pt), json!({})).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::GONE, "INVITATION_INVALID"),
        "{v}"
    );
    // Registration admission token: one use.
    let (wsid, wcid) = app
        .live_conference(
            &owner,
            json!({ "title": "webinar", "accessPolicy": "INVITE_ONLY", "meetingType": "WEBINAR" }),
        )
        .await;
    let (s, reg) = app
        .post(
            format!("/api/v2/meeting-spaces/{wsid}/registrations"),
            Auth::None,
            json!({ "displayName": "Reg" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{reg}");
    let rtoken = reg["admissionToken"].as_str().unwrap().to_string();
    let used = app
        .join(
            wcid,
            Auth::None,
            json!({ "displayName": "Reg", "invitationToken": rtoken }),
        )
        .await;
    assert_eq!(used.0, StatusCode::OK, "{}", used.1);
    exec(&app, &format!("UPDATE meeting_invitations SET usage_count = use_limit WHERE token_hash IS NOT NULL AND meeting_space_id='{wsid}'")).await;
    let (s, v) = app
        .join(
            wcid,
            Auth::None,
            json!({ "displayName": "Reg", "invitationToken": rtoken }),
        )
        .await;
    assert!(
        s == StatusCode::GONE || s == StatusCode::FORBIDDEN,
        "consumed token: {s} {v}"
    );
}

/// An expired guest session: the guest credential identifies, but every reconnect
/// re-runs admission — no media from the expired session.
#[tokio::test]
async fn replay_expired_guest_session_reauthorizes() {
    let app = app!(TestApp::collab());
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    exec(&app, &format!("UPDATE participant_sessions SET expires_at = now() - interval '1 second' WHERE id='{}'", g[0].session_id)).await;
    // Guest is removed meanwhile: re-authorization must apply, not the old session.
    app.post(
        format!(
            "/api/v2/conferences/{cid}/participants/{}/remove",
            g[0].participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    let (s, v) = app
        .join(cid, g[0].auth(), json!({ "displayName": "g" }))
        .await;
    assert!(
        s.is_client_error(),
        "re-authorization refused after removal: {s} {v}"
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_ne!(session_state(&app, g[0].session_id).await, "CONNECTED");
}
