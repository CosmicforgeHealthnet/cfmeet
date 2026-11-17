//! Blueprint v1.0 Sections 2-3 hardening, against a real PostgreSQL (and Redis where the
//! v1 middleware needs it).
//!
//! * PLT-002 / SEC-006 / DATA-003: legacy API keys and webhook secrets are not stored;
//!   keys verify by hash, the plaintext fallback heals old rows, the quota is atomic.
//! * SEC-009: a legacy kick removes the participant from the media room server-side, and a
//!   reconnect with an earlier credential is removed again by the verified webhook.
//! * ACC-026: self-deactivation needs a step-up code emailed to the account; both
//!   outcomes and the administrator reactivation are audited.
//! * ACC-016: the (off by default) email-verification enforcement points; a sign-in code
//!   verifies a pending account.
//! * Passwordless cutover: the password endpoints no longer exist.
mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

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

async fn email_user(app: &TestApp, email: &str, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    app.db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id, username, email, auth_type, role, status, created_at, updated_at) \
             VALUES ($1, $2, $3, 'local', 'user', $4::user_status, now(), now())",
            [
                id.into(),
                format!("h_{}", &id.simple().to_string()[..10]).into(),
                email.into(),
                status.into(),
            ],
        ))
        .await
        .unwrap();
    id
}

/// Passwordless sign-in with a seeded code: the raw `/otp/verify` answer.
async fn login(app: &TestApp, email: &str) -> (StatusCode, Value) {
    app.seed_sign_in_code(email, TEST_SIGN_IN_CODE).await;
    app.post(
        "/api/v1/auth/otp/verify",
        Auth::None,
        json!({ "email": email, "code": TEST_SIGN_IN_CODE }),
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
async fn deactivation_requires_a_step_up_code() {
    let app = app_with_redis!();
    let id = email_user(&app, "deact@example.com", "active").await;
    let (s, v) = login(&app, "deact@example.com").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let access = v["access_token"].as_str().unwrap().to_string();
    let stale = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();

    // A session token alone, a made-up token, or a pre-session token is not enough.
    for (token, body) in [
        (&access, json!({})),
        (&access, json!({ "step_up_token": "not-a-token" })),
        (&stale, json!({})),
    ] {
        let (s, v) = app
            .post("/api/v1/users/me/deactivate", Auth::Bearer(token), body)
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
        assert_eq!(code(&v), "REAUTHENTICATION_REQUIRED");
    }
    assert_eq!(status_of(&app, id).await, "active");
    assert_eq!(events(&app, id, "ACCOUNT_DEACTIVATED", "DENIED").await, 3);

    // A token for another purpose does not work, and is used up by the attempt.
    let wrong_purpose = app.step_up_token(&access, id, "CHANGE_EMAIL").await;
    let (s, _) = app
        .post(
            "/api/v1/users/me/deactivate",
            Auth::Bearer(&access),
            json!({ "step_up_token": wrong_purpose }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // A token from another session of the same account does not work either.
    let (_, other) = login(&app, "deact@example.com").await;
    let other_access = other["access_token"].as_str().unwrap().to_string();
    let foreign = app
        .step_up_token(&other_access, id, "DEACTIVATE_ACCOUNT")
        .await;
    let (s, _) = app
        .post(
            "/api/v1/users/me/deactivate",
            Auth::Bearer(&access),
            json!({ "step_up_token": foreign }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(status_of(&app, id).await, "active");

    // The right token, from this session, deactivates; it is single use.
    let token = app.step_up_token(&access, id, "DEACTIVATE_ACCOUNT").await;
    let (s, v) = app
        .post(
            "/api/v1/users/me/deactivate",
            Auth::Bearer(&access),
            json!({ "step_up_token": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(status_of(&app, id).await, "inactive");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' \
             AND event_type = 'ACCOUNT_DEACTIVATED' AND outcome = 'SUCCESS' \
             AND detail->>'reauthentication' = 'STEP_UP'"
        ))
        .await,
        1
    );
    // Every token ended with it, and a deactivated account cannot sign in.
    for t in [&access, &stale] {
        let (s, _) = app.get("/api/v1/users/me", Auth::Bearer(t)).await;
        assert!(s.is_client_error());
    }
    let (s, _) = login(&app, "deact@example.com").await;
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
    let (s, v) = login(&app, "deact@example.com").await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn step_up_codes_are_mailed_limited_and_need_a_session() {
    let app = app_with_redis!();
    let id = email_user(&app, "stepup@example.com", "active").await;
    let (_, v) = login(&app, "stepup@example.com").await;
    let access = v["access_token"].as_str().unwrap().to_string();
    let (s, v) = app
        .post(
            "/api/v2/me/step-up/request",
            Auth::Bearer(&access),
            json!({ "purpose": "DEACTIVATE_ACCOUNT" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["expiresInSecs"], 600);
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM email_deliveries WHERE category = 'SECURITY_DEVICE_STEP_UP' \
              AND idempotency_key LIKE 'otp:stepup:%'"
        )
        .await,
        1
    );
    // Wrong code: 400; unknown purpose: 400; pre-session tokens are refused.
    let (s, _) = app
        .post(
            "/api/v2/me/step-up/verify",
            Auth::Bearer(&access),
            json!({ "purpose": "DEACTIVATE_ACCOUNT", "code": "000000" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app
        .post(
            "/api/v2/me/step-up/request",
            Auth::Bearer(&access),
            json!({ "purpose": "DELETE_EVERYTHING" }),
        )
        .await;
    assert!(s.is_client_error());
    let stale = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();
    let (s, v) = app
        .post(
            "/api/v2/me/step-up/request",
            Auth::Bearer(&stale),
            json!({ "purpose": "DEACTIVATE_ACCOUNT" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    // At most five codes per purpose per window.
    for _ in 0..4 {
        app.post(
            "/api/v2/me/step-up/request",
            Auth::Bearer(&access),
            json!({ "purpose": "DEACTIVATE_ACCOUNT" }),
        )
        .await;
    }
    let (s, _) = app
        .post(
            "/api/v2/me/step-up/request",
            Auth::Bearer(&access),
            json!({ "purpose": "DEACTIVATE_ACCOUNT" }),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
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
async fn a_sign_in_code_verifies_a_pending_account() {
    let app = app_with_redis!();
    // A legacy registration that never verified its address. Even with the strictest
    // enforcement, the code proves the address: the account becomes active and verified.
    let app = app.with_auth_env(&[("EMAIL_VERIFICATION_ENFORCEMENT", "login")]);
    let id = email_user(&app, "pending@example.com", "pending_verification").await;
    let (s, v) = login(&app, "pending@example.com").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["user"]["status"], "active");
    assert_eq!(v["is_new_account"], false);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM users WHERE id = '{id}' AND status::text = 'active' \
             AND email_verified_at IS NOT NULL"
        ))
        .await,
        1
    );
    assert_eq!(events(&app, id, "EMAIL_VERIFIED", "SUCCESS").await, 1);
}

#[tokio::test]
async fn communications_refuse_unverified_accounts_only_when_enforced() {
    let app = app_with_redis!();
    // A pending account still holding a token from before the cutover.
    let id = email_user(&app, "pendingcomms@example.com", "pending_verification").await;
    let token = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();
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
// Passwordless cutover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_password_endpoints_are_gone() {
    let app = app_with_redis!();
    let user = app.user("cutover").await;
    for path in [
        "/api/v1/auth/register",
        "/api/v1/auth/login",
        "/api/v1/auth/verify-email",
        "/api/v1/auth/resend-verification",
        "/api/v1/auth/forgot-password",
        "/api/v1/auth/verify-reset-token",
        "/api/v1/auth/reset-password",
        "/api/v1/auth/change-password",
    ] {
        let (s, _) = app
            .post(
                path,
                Auth::Bearer(&user.token),
                json!({ "email": user.email, "password": "Str0ng!Passw0rd" }),
            )
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
    }
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
