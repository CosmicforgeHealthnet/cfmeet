//! Blueprint v1.0 Sections 2-3 hardening, against a real PostgreSQL (and Redis where the
//! v1 middleware needs it).
//!
//! * PLT-002 / SEC-006 / DATA-003: legacy API keys and webhook secrets are not stored;
//!   keys verify by hash, the plaintext fallback heals old rows, the quota is atomic.
//! * SEC-009: a legacy kick removes the participant from the media room server-side, and a
//!   reconnect with an earlier credential is removed again by the verified webhook.
//! * ACC-026: self-deactivation needs the current password or a recent sign-in; both
//!   outcomes and the administrator reactivation are audited.
//! * ACC-016: the (off by default) email-verification enforcement points.
//! * ACC-025: passwords beyond the bcrypt input limit are refused.
mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const PASSWORD: &str = "Str0ng!Passw0rd";

macro_rules! app_with_redis {
    () => {{
        if !require_test_redis() {
            return;
        }
        match TestApp::new().await {
            Some(app) => app,
            None => return,
        }
    }};
}

async fn password_user(app: &TestApp, email: &str, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    let hash = backend::services::auth::hash_password(PASSWORD).unwrap();
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id, username, email, password_hash, auth_type, role, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 'local', 'user', $5::user_status, now(), now())",
            [
                id.into(),
                format!("h_{}", &id.simple().to_string()[..10]).into(),
                email.into(),
                hash.into(),
                status.into(),
            ],
        ))
        .await
        .unwrap();
    id
}

async fn login(app: &TestApp, email: &str) -> (StatusCode, Value) {
    app.post(
        "/api/v1/auth/login",
        Auth::None,
        json!({ "email": email, "password": PASSWORD }),
    )
    .await
}

async fn text(app: &TestApp, sql: &str) -> Option<String> {
    app.db
        .conn
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
}

async fn call_with_key(app: &TestApp, key: &str) -> StatusCode {
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/api/meetings")
        .header("content-type", "application/json")
        .header("Api-Key", key)
        .body(axum::body::Body::from("{}"))
        .unwrap();
    use tower::ServiceExt;
    app.router.clone().oneshot(req).await.unwrap().status()
}

fn authenticated(s: StatusCode) -> bool {
    s != StatusCode::UNAUTHORIZED && s != StatusCode::FORBIDDEN
}

// ---------------------------------------------------------------------------
// PLT-002 / SEC-006 / DATA-003
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_api_keys_are_stored_as_verifiers_and_still_authenticate() {
    let app = app_with_redis!();
    let owner = app.user("keyowner").await;
    let (s, v) = app
        .post("/api/v1/api-keys", Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let key = v["data"]["api_key"].as_str().unwrap().to_string();
    let id = v["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(key.len(), 64);

    // Nothing usable is stored: a placeholder, a domain-separated verifier, the prefix.
    let stored = text(
        &app,
        &format!("SELECT api_key FROM api_keys WHERE id = '{id}'"),
    )
    .await
    .unwrap();
    assert!(!stored.contains(&key[8..]), "the key is stored");
    assert!(stored.starts_with('\u{1}'));
    assert_eq!(
        text(
            &app,
            &format!("SELECT key_hash FROM api_keys WHERE id = '{id}'")
        )
        .await,
        Some(backend::services::legacy_credentials::api_key_hash(&key))
    );
    assert_eq!(
        text(
            &app,
            &format!("SELECT key_prefix FROM api_keys WHERE id = '{id}'")
        )
        .await,
        Some(key[..8].to_string())
    );

    // Reads show only the prefix.
    let (s, v) = app
        .get(format!("/api/v1/api-keys/{id}"), Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let masked = v["data"]["api_key_masked"].as_str().unwrap();
    assert_eq!(masked, format!("{}********", &key[..8]));

    // The key authenticates; a wrong key with the same prefix and the stored placeholder do not.
    assert!(authenticated(call_with_key(&app, &key).await));
    let wrong = format!("{}{}", &key[..8], "Z".repeat(56));
    assert_eq!(call_with_key(&app, &wrong).await, StatusCode::UNAUTHORIZED);
    assert_eq!(call_with_key(&app, "short").await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.scalar(&format!(
            "SELECT used_count::bigint FROM api_keys WHERE id = '{id}'"
        ))
        .await,
        1
    );

    // Revoked keys stop working.
    let (s, _) = app
        .delete(format!("/api/v1/api-keys/{id}"), Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(call_with_key(&app, &key).await, StatusCode::UNAUTHORIZED);
}

/// A row written by an older image after the migration (no verifier) still works once
/// through the plaintext fallback and is healed; the backfill format equals the service's.
#[tokio::test]
async fn plaintext_rows_are_found_once_and_healed() {
    let app = app_with_redis!();
    let owner = app.user("legacykey").await;
    let key: String = "Lg".repeat(32);
    let id = Uuid::new_v4();
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO api_keys (id, user_id, api_key, usage_limit, used_count, expires_at, status, created_at, updated_at) \
             VALUES ($1, $2, $3, 100, 0, now() + interval '1 day', 'active', now(), now())",
            [id.into(), owner.id.into(), key.clone().into()],
        ))
        .await
        .unwrap();
    assert!(authenticated(call_with_key(&app, &key).await));
    assert_eq!(
        text(
            &app,
            &format!("SELECT key_hash FROM api_keys WHERE id = '{id}'")
        )
        .await,
        Some(backend::services::legacy_credentials::api_key_hash(&key))
    );
    // The migration's SQL backfill produces the same verifier.
    assert_eq!(
        text(
            &app,
            &format!(
                "SELECT encode(sha256(convert_to('cfmeet-legacy-api-key:v1|' || '{key}', 'UTF8')), 'hex')"
            )
        )
        .await,
        Some(backend::services::legacy_credentials::api_key_hash(&key))
    );
    assert!(authenticated(call_with_key(&app, &key).await));
    assert_eq!(
        app.scalar(&format!(
            "SELECT used_count::bigint FROM api_keys WHERE id = '{id}'"
        ))
        .await,
        2
    );
}

/// The quota is consumed atomically: concurrent requests never exceed it.
#[tokio::test]
async fn api_key_quota_is_atomic_under_concurrency() {
    let app = app_with_redis!();
    let owner = app.user("quota").await;
    let (s, v) = app
        .post("/api/v1/api-keys", Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let key = v["data"]["api_key"].as_str().unwrap().to_string();
    let id = v["data"]["id"].as_str().unwrap().to_string();
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE api_keys SET usage_limit = 5 WHERE id = '{id}'"
        ))
        .await
        .unwrap();
    let app = Arc::new(app);
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let (app, key) = (app.clone(), key.clone());
        tasks.push(tokio::spawn(async move { call_with_key(&app, &key).await }));
    }
    let mut ok = 0;
    let mut over = 0;
    for t in tasks {
        match t.await.unwrap() {
            StatusCode::FORBIDDEN => over += 1,
            s if authenticated(s) => ok += 1,
            s => panic!("unexpected {s}"),
        }
    }
    assert_eq!((ok, over), (5, 15));
    assert_eq!(
        app.scalar(&format!(
            "SELECT used_count::bigint FROM api_keys WHERE id = '{id}'"
        ))
        .await,
        5
    );
}

#[tokio::test]
async fn legacy_webhook_secrets_are_not_stored() {
    let app = app_with_redis!();
    let owner = app.user("hooks").await;
    let (s, v) = app
        .post(
            "/api/v1/webhooks",
            Auth::Bearer(&owner.token),
            json!({ "event_type": "meeting_start", "endpoint_url": "https://hooks.example.test/x" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let secret = v["data"]["secret"].as_str().unwrap().to_string();
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let stored = text(
        &app,
        &format!("SELECT secret FROM webhooks WHERE id = '{id}'"),
    )
    .await
    .unwrap();
    assert_ne!(stored, secret);
    assert!(stored.starts_with('\u{1}'));
    assert_eq!(
        text(
            &app,
            &format!("SELECT secret_sha256 FROM webhooks WHERE id = '{id}'")
        )
        .await,
        Some(backend::services::legacy_credentials::webhook_secret_fingerprint(&secret))
    );
}

// ---------------------------------------------------------------------------
// SEC-009
// ---------------------------------------------------------------------------

async fn legacy_meeting_with_guest(app: &TestApp, host: &User) -> (Uuid, String, Uuid) {
    let id = Uuid::new_v4();
    let code = format!("H{}", &id.simple().to_string()[..7])
        .to_uppercase()
        .replace(['0', '1', 'O', 'I'], "Q");
    let participant = Uuid::new_v4();
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{id}', '{code}', '{h}', '{h}', 'legacy', false, now() - interval '5 minutes', now() + interval '1 hour', 'ongoing'); \
             INSERT INTO participants (id, meeting_id, user_id, role, join_time, status, is_muted, is_video_on, is_screen_sharing, display_name) \
             VALUES ('{participant}', '{id}', NULL, 'participant', now(), 'joined', false, false, false, 'Guest')",
            h = host.id
        ))
        .await
        .unwrap();
    (id, code, participant)
}

#[tokio::test]
async fn legacy_kick_removes_the_participant_from_the_media_room() {
    let app = app_with_redis!();
    let host = app.user("kickhost").await;
    let (_, code, participant) = legacy_meeting_with_guest(&app, &host).await;
    let (s, v) = app
        .post(
            &format!("/api/v1/participants/{participant}/kick"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let removed = app.rtc.removed.lock().unwrap().clone();
    assert_eq!(
        removed,
        vec![(
            code.clone(),
            participant.to_string(),
            "LEGACY_KICK".to_string()
        )]
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM session_logs WHERE participant_id = '{participant}' \
             AND metadata->>'kicked' = 'true' AND metadata->>'media_evicted' = 'true'"
        ))
        .await,
        1
    );
    // A non-host cannot kick (and nothing is removed).
    let other = app.user("notthehost").await;
    let (_, _, p2) = legacy_meeting_with_guest(&app, &host).await;
    let (s, _) = app
        .post(
            &format!("/api/v1/participants/{p2}/kick"),
            Auth::Bearer(&other.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(app.rtc.removed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_kicked_legacy_participant_reconnecting_is_removed_again() {
    if !require_test_redis() {
        return;
    }
    let Some(app) = TestApp::livekit().await else {
        return;
    };
    let host = app.user("rejoinhost").await;
    let (_, code, participant) = legacy_meeting_with_guest(&app, &host).await;
    let (s, v) = app
        .post(
            &format!("/api/v1/participants/{participant}/kick"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let lk = app.lk.as_ref().unwrap();
    let before = lk.call_count("RemoveParticipant");
    assert!(before >= 1, "the kick reached the provider");
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &code,
        None,
        Some(&participant.to_string()),
    );
    let (s, v) = app.signed_webhook(&body).await;
    assert!(s.is_success(), "{s} {v}");
    assert_eq!(lk.call_count("RemoveParticipant"), before + 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM rtc_webhook_events WHERE room_name = '{code}' AND outcome = 'LEGACY_REMOVED_EVICTED'"
        ))
        .await,
        1
    );
    // A participant who was not kicked is left alone.
    let (_, code2, p2) = legacy_meeting_with_guest(&app, &host).await;
    let body = livekit_mock::webhook_body(
        "participant_joined",
        &format!("EV_{}", Uuid::new_v4().simple()),
        &code2,
        None,
        Some(&p2.to_string()),
    );
    let (s, _) = app.signed_webhook(&body).await;
    assert!(s.is_success());
    assert_eq!(lk.call_count("RemoveParticipant"), before + 1);
}

// ---------------------------------------------------------------------------
// ACC-026
// ---------------------------------------------------------------------------

async fn status_of(app: &TestApp, id: Uuid) -> String {
    text(
        app,
        &format!("SELECT status::text FROM users WHERE id = '{id}'"),
    )
    .await
    .unwrap()
}

async fn events(app: &TestApp, id: Uuid, kind: &str, outcome: &str) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' \
         AND event_type = '{kind}' AND outcome = '{outcome}'"
    ))
    .await
}

#[tokio::test]
async fn deactivation_requires_reauthentication() {
    let app = app_with_redis!();
    let id = password_user(&app, "deact@example.com", "active").await;
    // A pre-session (or stolen, stale) token alone is not enough.
    let stale = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();
    for body in [json!({}), json!({ "current_password": "wrong-password" })] {
        let (s, v) = app
            .post("/api/v1/users/me/deactivate", Auth::Bearer(&stale), body)
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(code(&v), "REAUTHENTICATION_REQUIRED");
    }
    assert_eq!(status_of(&app, id).await, "active");
    assert_eq!(events(&app, id, "ACCOUNT_DEACTIVATED", "DENIED").await, 2);

    // The current password is enough.
    let (s, v) = app
        .post(
            "/api/v1/users/me/deactivate",
            Auth::Bearer(&stale),
            json!({ "current_password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(status_of(&app, id).await, "inactive");
    assert_eq!(events(&app, id, "ACCOUNT_DEACTIVATED", "SUCCESS").await, 1);
    // Every token ended with it.
    let (s, _) = app.get("/api/v1/users/me", Auth::Bearer(&stale)).await;
    assert!(s.is_client_error());
}

#[tokio::test]
async fn a_fresh_sign_in_is_recent_reauthentication_and_admins_can_reactivate() {
    let app = app_with_redis!();
    let id = password_user(&app, "fresh@example.com", "active").await;
    let (s, v) = login(&app, "fresh@example.com").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["access_token"].as_str().unwrap().to_string();
    // Without a body (the current web client) a sign-in made just now suffices.
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/users/me/deactivate")
        .header("authorization", format!("Bearer {token}"))
        .body(axum::body::Body::empty())
        .unwrap();
    use tower::ServiceExt;
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(status_of(&app, id).await, "inactive");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' \
             AND event_type = 'ACCOUNT_DEACTIVATED' AND detail->>'reauthentication' = 'RECENT_SIGN_IN'"
        ))
        .await,
        1
    );
    let (s, _) = login(&app, "fresh@example.com").await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a deactivated account cannot sign in"
    );

    // Administrator reactivation is the reverse path, and it is audited.
    let admin = app.admin("reactivator").await;
    let (s, v) = app
        .patch(
            format!("/api/v1/admin/users/{id}"),
            Auth::Bearer(&admin.token),
            json!({ "status": "active" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' \
             AND event_type = 'ACCOUNT_STATUS_CHANGED' AND actor_kind = 'ADMIN' \
             AND actor_user_id = '{}' AND detail->>'reactivated' = 'true'",
            admin.id
        ))
        .await,
        1
    );
    let (s, v) = login(&app, "fresh@example.com").await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn the_reauthentication_window_is_configurable_and_can_be_switched_off() {
    let Some(app) = ({
        if !require_test_redis() {
            return;
        }
        TestApp::new().await
    }) else {
        return;
    };
    let app = app.with_auth_env(&[("ACCOUNT_DEACTIVATION_REQUIRE_REAUTH", "off")]);
    let user = app.user("noreauth").await;
    let (s, v) = app
        .post(
            "/api/v1/users/me/deactivate",
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{}' \
             AND detail->>'reauthentication' = 'NOT_REQUIRED'",
            user.id
        ))
        .await,
        1
    );
}

// ---------------------------------------------------------------------------
// ACC-016 (owner decision; off by default)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pending_accounts_sign_in_unless_enforcement_is_switched_on() {
    let app = app_with_redis!();
    password_user(&app, "pending@example.com", "pending_verification").await;
    let (s, v) = login(&app, "pending@example.com").await;
    assert_eq!(s, StatusCode::OK, "default unchanged: {v}");

    let app = app.with_auth_env(&[("EMAIL_VERIFICATION_ENFORCEMENT", "login")]);
    let (s, v) = login(&app, "pending@example.com").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "EMAIL_VERIFICATION_REQUIRED");
    // A wrong password still gets the uniform answer (no verification-state oracle).
    let (s, _) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": "pending@example.com", "password": "Wr0ng!Password" }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Active accounts are unaffected.
    password_user(&app, "verified@example.com", "active").await;
    assert_eq!(login(&app, "verified@example.com").await.0, StatusCode::OK);
}

#[tokio::test]
async fn communications_refuse_unverified_accounts_only_when_enforced() {
    let app = app_with_redis!();
    password_user(&app, "pendingcomms@example.com", "pending_verification").await;
    // The deployment allows pending sign-in by default.
    let (s, v) = login(&app, "pendingcomms@example.com").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["access_token"].as_str().unwrap().to_string();
    // Default: the Hub relay is reached (not configured in this test: 503).
    let (s, v) = app
        .get("/api/v2/communications/me", Auth::Bearer(&token))
        .await;
    assert_ne!(code(&v), "EMAIL_VERIFICATION_REQUIRED", "{s} {v}");

    let app = app.with_auth_env(&[("EMAIL_VERIFICATION_ENFORCEMENT", "communications")]);
    let (s, v) = app
        .get("/api/v2/communications/me", Auth::Bearer(&token))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "EMAIL_VERIFICATION_REQUIRED");
}

// ---------------------------------------------------------------------------
// ACC-025
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registration_refuses_passwords_beyond_the_bcrypt_limit() {
    let app = app_with_redis!();
    let long = "Aa1!".repeat(19); // 76 bytes
    let (s, v) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            json!({
                "username": "longpassword",
                "email": "long@example.com",
                "password": long,
                "confirm_password": long,
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        app.scalar("SELECT count(*) FROM users WHERE email = 'long@example.com'")
            .await,
        0
    );
}

// ---------------------------------------------------------------------------
// DATA-017 (report only)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reference_reconciliation_reports_local_invariant_breaks_without_repairing() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let clean = backend::services::reference_reconciliation::report(&db.conn)
        .await
        .unwrap();
    assert_eq!(clean.anomalies, 0, "{clean:?}");
    assert!(clean.checks.iter().all(|c| c.status == "OK"));
    // A principal link whose account does not exist, and a chat member principal this
    // service never mapped.
    let (env, principal, ghost) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    db.conn
        .execute_unprepared(&format!(
            "INSERT INTO communication_principal_links (subject_kind, subject_id, environment_id, principal_id, principal_kind) \
             VALUES ('USER', '{ghost}', '{env}', '{principal}', 'NATIVE_CF_USER')"
        ))
        .await
        .unwrap();
    let r = backend::services::reference_reconciliation::report(&db.conn)
        .await
        .unwrap();
    let links = r
        .checks
        .iter()
        .find(|c| c.name == "principal_links_to_accounts")
        .unwrap();
    assert_eq!(
        (links.rows, links.distinct_foreign_ids, links.anomalies),
        (1, 1, 1)
    );
    assert_eq!(links.status, "ATTENTION");
    assert_eq!(r.anomalies, 1);
    assert!(r.remote_verification.starts_with("NOT_PERFORMED"));
    // Report only: the row is still there.
    let n = db
        .conn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) FROM communication_principal_links".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap();
    assert_eq!(n, 1);
}
