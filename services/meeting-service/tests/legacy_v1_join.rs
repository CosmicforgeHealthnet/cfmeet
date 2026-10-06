//! Permanent regression suite for the legacy v1 join identity fix.
//!
//! `POST /api/v1/meetings/{id}/join` and `POST /api/v1/meetings/join/{code}` keep their
//! links and response contract, but registered identity now comes only from a verified
//! bearer token and guest continuity only from the server-signed guest token.

mod common;

use axum::http::StatusCode;
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
}

/// Insert a live legacy meeting. Returns (id, code).
async fn legacy_meeting(app: &TestApp, host: &User, private: bool) -> (Uuid, String) {
    let id = Uuid::new_v4();
    let code: String = format!("V{}", &id.simple().to_string()[..7])
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

async fn count(app: &TestApp, sql: &str) -> i64 {
    app.scalar(sql).await
}

/// Unauthenticated v1 caller cannot impersonate a registered user.
#[tokio::test]
async fn v1_unauthenticated_cannot_impersonate_registered_user() {
    let app = app!();
    let host = app.user("host").await;
    let victim = app.user("victim").await;
    let (id, code) = legacy_meeting(&app, &host, false).await;
    for uri in [
        format!("/api/v1/meetings/{id}/join"),
        format!("/api/v1/meetings/join/{code}"),
    ] {
        let (s, v) = app
            .post(
                &uri,
                Auth::None,
                json!({ "user_id": victim.id, "display_name": "victim" }),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    }
    // Private meeting: the claim also cannot get past the guest restriction.
    let (pid, _) = legacy_meeting(&app, &host, true).await;
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/{pid}/join"),
            Auth::None,
            json!({ "user_id": victim.id, "display_name": "x" }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        count(
            &app,
            &format!(
                "SELECT count(*) FROM participants WHERE user_id='{}'",
                victim.id
            )
        )
        .await,
        0
    );
    // A forged token for the victim is rejected outright.
    let forged = backend::services::auth::JwtService::new("not-the-server-secret".into())
        .generate_access_token(victim.id, "user")
        .unwrap();
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&forged),
            json!({ "display_name": "x" }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

/// A participant cannot become host by sending the host's user_id.
#[tokio::test]
async fn v1_participant_cannot_become_host_via_user_id() {
    let app = app!();
    let host = app.user("host").await;
    let attacker = app.user("attacker").await;
    let (id, _) = legacy_meeting(&app, &host, false).await;
    // Authenticated as themself, claiming the host's id → rejected.
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&attacker.token),
            json!({ "user_id": host.id, "display_name": "host" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // Anonymous, claiming the host's id → rejected.
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::None,
            json!({ "user_id": host.id, "display_name": "host" }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Joining as themself works, as a plain participant.
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&attacker.token),
            json!({ "user_id": attacker.id, "display_name": "a" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["role"], "participant");
    // A legacy guest token can never claim a user identity.
    let guest_token = app
        .state
        .jwt_service
        .generate_access_token(Uuid::new_v4(), "guest")
        .unwrap();
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&guest_token),
            json!({ "user_id": host.id, "display_name": "h" }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM participants WHERE meeting_id='{id}' AND role='host'")
        )
        .await,
        0,
        "nobody became host"
    );
    // The real, authenticated host does get the host role.
    let (s, v) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&host.token),
            json!({ "display_name": "Host" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["role"], "host");
}

/// Legacy guest behaviour still works, and guests are never matched by display name.
#[tokio::test]
async fn v1_guest_behaviour_is_preserved_and_safe() {
    let app = app!();
    let host = app.user("host").await;
    let (id, code) = legacy_meeting(&app, &host, false).await;
    let (s, g1) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            json!({ "display_name": "Guest" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{g1}");
    let p1 = g1["data"]["participant_id"].as_str().unwrap().to_string();
    let guest_token = g1["data"]["access_token"].as_str().unwrap().to_string();
    assert!(g1["data"]["refresh_token"].is_string());
    assert!(!g1["data"]["join_token"].as_str().unwrap().is_empty());
    assert_eq!(g1["data"]["role"], "participant");
    // Rejoin with the guest token resumes the same participant.
    let (s, again) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&guest_token),
            json!({ "display_name": "Guest" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["data"]["participant_id"], p1);
    // Someone copying the display name gets a NEW participant and none of the first guest's credentials.
    let (_, copy) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::None,
            json!({ "display_name": "Guest" }),
        )
        .await;
    assert_ne!(copy["data"]["participant_id"], p1);
    assert_ne!(copy["data"]["access_token"], g1["data"]["access_token"]);
    // A guest token for another meeting does not resume anything here.
    let (other, _) = legacy_meeting(&app, &host, false).await;
    let (_, cross) = app
        .post(
            &format!("/api/v1/meetings/{other}/join"),
            Auth::Bearer(&guest_token),
            json!({ "display_name": "Guest" }),
        )
        .await;
    assert_ne!(cross["data"]["participant_id"], p1);
    // Private meetings still refuse guests.
    let (private, _) = legacy_meeting(&app, &host, true).await;
    let (s, _) = app
        .post(
            &format!("/api/v1/meetings/{private}/join"),
            Auth::None,
            json!({ "display_name": "g" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(
        count(
            &app,
            &format!(
                "SELECT count(*) FROM participants WHERE meeting_id='{id}' AND user_id IS NULL"
            )
        )
        .await,
        2
    );
}

/// Old links (UUID and code) keep working with the unchanged response contract.
#[tokio::test]
async fn v1_old_links_keep_working() {
    let app = app!();
    let host = app.user("host").await;
    let member = app.user("member").await;
    let (id, code) = legacy_meeting(&app, &host, true).await;
    // Registered users authenticate with the bearer token; user_id remains accepted when it matches.
    let (s, by_code) = app
        .post(
            &format!("/api/v1/meetings/join/{code}"),
            Auth::Bearer(&member.token),
            json!({ "user_id": member.id, "display_name": "Member" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{by_code}");
    for field in [
        "meeting_id",
        "participant_id",
        "role",
        "join_token",
        "livekit_url",
        "room_name",
    ] {
        assert!(by_code["data"].get(field).is_some(), "missing {field}");
    }
    assert_eq!(
        by_code["data"]["join_token"], "",
        "private meeting: waiting, no media token"
    );
    // Polling with the same identity returns the same participant (no duplicates).
    let (_, poll) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&member.token),
            json!({ "display_name": "Member" }),
        )
        .await;
    assert_eq!(
        poll["data"]["participant_id"],
        by_code["data"]["participant_id"]
    );
    assert_eq!(
        count(
            &app,
            &format!("SELECT count(*) FROM participants WHERE meeting_id='{id}'")
        )
        .await,
        1
    );
    let (s, host_join) = app
        .post(
            &format!("/api/v1/meetings/{id}/join"),
            Auth::Bearer(&host.token),
            json!({ "user_id": host.id, "display_name": "Host" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(host_join["data"]["room_name"], code);
    assert!(!host_join["data"]["join_token"].as_str().unwrap().is_empty());
    let (s, _) = app
        .get(&format!("/api/v1/meetings/public/{code}"), Auth::None)
        .await;
    assert_eq!(s, StatusCode::OK);
}
