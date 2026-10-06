//! Web-client compatibility suite for the legacy v1 join (final A–D compatibility closure).
//!
//! Emulates exactly what `apps/web` does after the cross-spec compatibility patch:
//! `meetingService.joinMeeting` goes through the authenticated `api` axios client, which
//! sends `Authorization: Bearer <cookie access_token>` (the signed-in user's token, or the
//! server-issued guest token persisted by the pre-join screen) together with the body
//! `{ user_id: profile?.id, display_name }`, polls every 3s while waiting, and on a 401
//! refreshes through `POST /api/v1/auth/refresh` and retries once.
//!
//! Identity authority is unchanged: body `user_id` is only a consistency check against
//! the verified token, and guests are never matched by display name.

mod common;

use axum::http::StatusCode;
use backend::services::auth::Claims;
use chrono::Utc;
use common::*;
use jsonwebtoken::{encode, EncodingKey, Header};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
}

/// Number of 3-second polls emulated by the waiting-room tests.
const POLLS: usize = 6;

/// Insert a live legacy meeting. Returns (id, code).
async fn legacy_meeting(app: &TestApp, host: &User, private: bool) -> (Uuid, String) {
    let id = Uuid::new_v4();
    let code: String = format!("W{}", &id.simple().to_string()[..7])
        .to_uppercase()
        .replace(['0', '1', 'O', 'I'], "Q");
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{id}', '{code}', '{h}', '{h}', 'legacy', {private}, now() - interval '5 minutes', now() + interval '1 hour', 'ongoing')",
            h = host.id
        ))
        .await
        .unwrap();
    (id, code)
}

/// A token signed with the server secret ("test-jwt-secret") that expired an hour ago.
fn expired_token(sub: Uuid, role: &str) -> String {
    let now = Utc::now().timestamp();
    encode(
        &Header::default(),
        &Claims {
            sub: sub.to_string(),
            role: role.into(),
            exp: now - 3600,
            iat: now - 7200,
        },
        &EncodingKey::from_secret(b"test-jwt-secret"),
    )
    .unwrap()
}

/// What the web `api` refresh interceptor does on a 401.
async fn refresh(app: &TestApp, refresh_token: &str) -> String {
    let (s, v) = app
        .post(
            "/api/v1/auth/refresh",
            Auth::None,
            json!({ "refresh_token": refresh_token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["access_token"]
        .as_str()
        .or_else(|| v["data"]["access_token"].as_str())
        .expect("refresh returns an access token")
        .to_string()
}

fn web_body(user_id: Option<Uuid>, display_name: &str) -> Value {
    match user_id {
        Some(id) => json!({ "user_id": id, "display_name": display_name }),
        None => json!({ "display_name": display_name }),
    }
}

/// Signed-in web users join legacy meetings with their authenticated identity, on both
/// link formats, and waiting-room polling resolves the same participant.
#[tokio::test]
async fn web_signed_in_user_join_works() {
    let app = app!();
    let host = app.user("host").await;
    let member = app.user("member").await;

    // Public meeting, code link: joined immediately as themself.
    let (pub_id, pub_code) = legacy_meeting(&app, &host, false).await;
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/join/{pub_code}"),
            Auth::Bearer(&member.token),
            web_body(Some(member.id), "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["role"], "participant");
    assert!(!v["data"]["join_token"].as_str().unwrap().is_empty());
    // Registered users never receive guest tokens, so the web client keeps its own session cookie.
    assert!(v["data"]["access_token"].is_null(), "{v}");
    assert!(v["data"]["refresh_token"].is_null(), "{v}");
    let p = v["data"]["participant_id"].clone();
    // Same user via the UUID link (e.g. rejoin after refresh) → same participant.
    let (s, again) = app
        .post(
            &format!("/api/v1/meetings/{pub_id}/join"),
            Auth::Bearer(&member.token),
            web_body(Some(member.id), "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["data"]["participant_id"], p);
    // Profile not loaded yet (no body user_id): the bearer token alone identifies them.
    let (s, no_profile) = app
        .post(
            &format!("/api/v1/meetings/join/{pub_code}"),
            Auth::Bearer(&member.token),
            web_body(None, "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(no_profile["data"]["participant_id"], p);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{pub_id}' AND user_id='{}'",
            member.id
        ))
        .await,
        1
    );

    // Private meeting: the signed-in user waits, and every poll returns the same participant.
    let (priv_id, priv_code) = legacy_meeting(&app, &host, true).await;
    let (s, first) = app
        .post(
            &format!("/api/v1/meetings/join/{priv_code}"),
            Auth::Bearer(&member.token),
            web_body(Some(member.id), "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["join_token"], "", "waiting room");
    let waiting = first["data"]["participant_id"].clone();
    for _ in 0..POLLS {
        let (s, poll) = app
            .post(
                &format!("/api/v1/meetings/join/{priv_code}"),
                Auth::Bearer(&member.token),
                web_body(Some(member.id), "Member"),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{poll}");
        assert_eq!(poll["data"]["participant_id"], waiting);
        assert_eq!(poll["data"]["join_token"], "");
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{priv_id}'"
        ))
        .await,
        1,
        "polling never duplicates the waiting participant"
    );
    // Host admits them; the next poll returns a media token for the same participant.
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE participants SET status='joined' WHERE id='{}'",
            waiting.as_str().unwrap()
        ))
        .await
        .unwrap();
    let (s, admitted) = app
        .post(
            &format!("/api/v1/meetings/join/{priv_code}"),
            Auth::Bearer(&member.token),
            web_body(Some(member.id), "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(admitted["data"]["participant_id"], waiting);
    assert!(!admitted["data"]["join_token"].as_str().unwrap().is_empty());
}

/// An expired signed-in session gets 401 (which triggers the web client's refresh
/// interceptor), and the retried request with the refreshed token succeeds.
#[tokio::test]
async fn web_signed_in_expired_token_refreshes_then_joins() {
    let app = app!();
    let host = app.user("host").await;
    let member = app.user("member").await;
    let (id, code) = legacy_meeting(&app, &host, false).await;
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&expired_token(member.id, "user")),
            web_body(Some(member.id), "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    let refresh_token = app
        .state
        .jwt_service
        .generate_refresh_token(member.id, "user")
        .unwrap();
    let fresh = refresh(&app, &refresh_token).await;
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&fresh),
            web_body(Some(member.id), "Member"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{id}' AND user_id='{}'",
            member.id
        ))
        .await,
        1
    );
}

/// A guest's first join returns a server-issued guest identity; the web client persists
/// it and every later poll/rejoin presents it and resolves the same participant — also
/// across the 15-minute access-token expiry via the refresh token.
#[tokio::test]
async fn web_guest_polling_does_not_duplicate() {
    let app = app!();
    let host = app.user("host").await;
    let (id, code) = legacy_meeting(&app, &host, false).await;
    let (s, first) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            web_body(None, "Guest"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let p = first["data"]["participant_id"].clone();
    let guest_access = first["data"]["access_token"].as_str().unwrap().to_string();
    let guest_refresh = first["data"]["refresh_token"].as_str().unwrap().to_string();
    // The guest token's subject is the participant, with the guest role.
    let claims = app.state.jwt_service.verify_token(&guest_access).unwrap();
    assert_eq!(claims.sub, p.as_str().unwrap());
    assert_eq!(claims.role, "guest");

    for i in 0..POLLS {
        let uri = if i % 2 == 0 {
            format!("/api/v1/meetings/join/{code}")
        } else {
            format!("/api/v1/meetings/{id}/join")
        };
        let (s, poll) = app
            .post(&uri, Auth::Bearer(&guest_access), web_body(None, "Guest"))
            .await;
        assert_eq!(s, StatusCode::OK, "{poll}");
        assert_eq!(poll["data"]["participant_id"], p);
        assert_eq!(poll["data"]["role"], "participant");
    }

    // Access token expired mid-session: 401 → refresh with the guest refresh token → same participant.
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&expired_token(uuid(&p), "guest")),
            web_body(None, "Guest"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let refreshed = refresh(&app, &guest_refresh).await;
    let (s, after) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&refreshed),
            web_body(None, "Guest"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{after}");
    assert_eq!(after["data"]["participant_id"], p);

    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{id}'"
        ))
        .await,
        1,
        "exactly one guest participant after first join + {POLLS} polls + refresh"
    );
}

/// Copying another participant's display name never yields their participant,
/// credentials or role.
#[tokio::test]
async fn web_guest_display_name_copy_does_not_impersonate() {
    let app = app!();
    let host = app.user("host").await;
    let member = app.user("member").await;
    let (id, code) = legacy_meeting(&app, &host, false).await;
    let (_, host_join) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&host.token),
            web_body(Some(host.id), "Dr Host"),
        )
        .await;
    let (_, member_join) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&member.token),
            web_body(Some(member.id), "Member"),
        )
        .await;
    let (_, guest) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            web_body(None, "Guest"),
        )
        .await;
    let taken = [
        host_join["data"]["participant_id"].clone(),
        member_join["data"]["participant_id"].clone(),
        guest["data"]["participant_id"].clone(),
    ];

    // A fresh browser (no cookie) and a browser holding its own guest token both copy names.
    let (_, attacker) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            web_body(None, "Attacker"),
        )
        .await;
    let attacker_token = attacker["data"]["access_token"]
        .as_str()
        .unwrap()
        .to_string();
    for name in ["Dr Host", "Member", "Guest"] {
        for auth in [Auth::None, Auth::Bearer(&attacker_token)] {
            let (s, copy) = app
                .post(
                    &format!("/api/v1/meetings/join/{code}"),
                    auth,
                    web_body(None, name),
                )
                .await;
            assert_eq!(s, StatusCode::OK, "{copy}");
            assert!(
                !taken.contains(&copy["data"]["participant_id"]),
                "display name {name} resolved someone else's participant"
            );
            assert_eq!(copy["data"]["role"], "participant");
            assert_ne!(copy["data"]["access_token"], guest["data"]["access_token"]);
        }
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{id}' AND role='host'"
        ))
        .await,
        1,
        "only the authenticated host holds the host role"
    );
}

/// Arbitrary body user_id never selects another registered actor, whatever the web
/// client's cookie holds.
#[tokio::test]
async fn web_arbitrary_user_id_cannot_impersonate() {
    let app = app!();
    let host = app.user("host").await;
    let victim = app.user("victim").await;
    let attacker = app.user("attacker").await;
    let (id, code) = legacy_meeting(&app, &host, true).await;
    let (_, g) = app
        .post(
            &format!(
                "/api/v1/meetings/join/{}",
                legacy_meeting(&app, &host, false).await.1
            ),
            Auth::None,
            web_body(None, "g"),
        )
        .await;
    let guest_token = g["data"]["access_token"].as_str().unwrap().to_string();
    let expired_victim = expired_token(victim.id, "user");
    for uri in [
        format!("/api/v1/meetings/join/{code}"),
        format!("/api/v1/meetings/{id}/join"),
    ] {
        for (auth, expected) in [
            (Auth::None, StatusCode::UNAUTHORIZED),
            (Auth::Bearer(&guest_token), StatusCode::UNAUTHORIZED),
            (Auth::Bearer(&expired_victim), StatusCode::UNAUTHORIZED),
            (Auth::Bearer("not-a-jwt"), StatusCode::UNAUTHORIZED),
            (Auth::Bearer(&attacker.token), StatusCode::FORBIDDEN),
        ] {
            let (s, v) = app
                .post(&uri, auth, web_body(Some(victim.id), "victim"))
                .await;
            assert_eq!(s, expected, "{uri}: {v}");
        }
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE user_id='{}'",
            victim.id
        ))
        .await,
        0
    );
}

/// The host role comes only from the authenticated host account.
#[tokio::test]
async fn web_host_role_only_from_authenticated_authority() {
    let app = app!();
    let host = app.user("host").await;
    let other = app.user("other").await;
    let (id, code) = legacy_meeting(&app, &host, false).await;
    // Every non-authoritative way of claiming to be the host fails or yields a participant.
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            web_body(Some(host.id), "Host"),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&other.token),
            web_body(Some(host.id), "Host"),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&other.token),
            json!({ "display_name": "Host", "role": "host", "is_host": true }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["role"], "participant");
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            json!({ "display_name": "Host", "role": "host" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["role"], "participant");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participants WHERE meeting_id='{id}' AND role='host'"
        ))
        .await,
        0
    );
    // The host's own session (as the web client sends it) gets the host role.
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&host.token),
            web_body(Some(host.id), "Host"),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["role"], "host");
    assert!(v["data"]["access_token"].is_null());
}
