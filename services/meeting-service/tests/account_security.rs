//! Blueprint v1.0 Section 2 — account security, against a real PostgreSQL and Redis.
//!
//! * ACC-001: typed tokens (refresh tokens never authenticate, access tokens never refresh)
//! * ACC-002/003: the revocation epoch applies on v1, v2 and refresh, without Communications
//! * ACC-007/017: sessions, per-session logout, rotation with reuse detection, legacy upgrade
//! * ACC-004: Google identity binding and the pending-account pre-hijack fix
//! * ACC-014 / DATA-002: uniform sign-in answers, case-insensitive email identity
//! * SEC-013: append-only security audit and its administrator read
mod common;

use axum::http::StatusCode;
use backend::handlers::auth::{resolve_google_account, GoogleIdentity};
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

/// An existing email account (no password: sign-in is by emailed code).
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
                format!("u_{}", &id.simple().to_string()[..10]).into(),
                email.into(),
                status.into(),
            ],
        ))
        .await
        .unwrap();
    id
}

async fn login(app: &TestApp, email: &str) -> (String, String) {
    app.otp_tokens(email).await
}

async fn refresh(app: &TestApp, token: &str) -> (StatusCode, Value) {
    app.post(
        "/api/v1/auth/refresh",
        Auth::None,
        json!({ "refresh_token": token }),
    )
    .await
}

/// v1 (`/api/v1/users/me`) and v2 (`/api/v2/me/sessions`) answers for one bearer.
async fn both_ingresses(app: &TestApp, token: &str) -> (StatusCode, StatusCode) {
    let v1 = app.get("/api/v1/users/me", Auth::Bearer(token)).await.0;
    let v2 = app.get("/api/v2/me/sessions", Auth::Bearer(token)).await.0;
    (v1, v2)
}

fn claims(app: &TestApp, token: &str) -> backend::services::auth::Claims {
    app.state.jwt_service.verify_token(token).unwrap()
}

#[tokio::test]
async fn login_opens_a_session_with_typed_tokens_and_case_insensitive_email() {
    let app = app_with_redis!();
    email_user(&app, "alice.typed@example.com", "active").await;
    let (access, refresh_token) = login(&app, "Alice.Typed@EXAMPLE.com").await;

    let a = claims(&app, &access);
    let r = claims(&app, &refresh_token);
    assert_eq!(a.typ.as_deref(), Some("access"));
    assert_eq!(r.typ.as_deref(), Some("refresh"));
    assert!(a.sid.is_some() && a.sid == r.sid);
    assert!(r.rti.is_some() && a.rti.is_none());

    // Access works on v1 and v2; a refresh token authenticates nowhere (ACC-001).
    assert_eq!(
        both_ingresses(&app, &access).await,
        (StatusCode::OK, StatusCode::OK)
    );
    assert_eq!(
        both_ingresses(&app, &refresh_token).await,
        (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    // An access token never refreshes.
    assert_eq!(refresh(&app, &access).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refresh_rotates_and_reuse_ends_the_whole_session() {
    let app = app_with_redis!();
    let id = email_user(&app, "rotate@example.com", "active").await;
    let (_, r1) = login(&app, "rotate@example.com").await;

    let (s, v) = refresh(&app, &r1).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let a2 = v["access_token"].as_str().unwrap().to_string();
    let r2 = v["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(claims(&app, &a2).sid, claims(&app, &r1).sid);
    assert_eq!(both_ingresses(&app, &a2).await.0, StatusCode::OK);

    // Replaying the superseded refresh token is reuse: the session ends for everyone.
    assert_eq!(refresh(&app, &r1).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(refresh(&app, &r2).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        both_ingresses(&app, &a2).await,
        (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE user_id = '{id}' AND revoke_reason = 'REFRESH_REUSE'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' AND event_type = 'REFRESH_TOKEN_REUSE'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn logout_ends_only_the_current_session() {
    let app = app_with_redis!();
    email_user(&app, "two.devices@example.com", "active").await;
    let (laptop, laptop_refresh) = login(&app, "two.devices@example.com").await;
    let (phone, _) = login(&app, "two.devices@example.com").await;

    let (s, v) = app
        .post("/api/v1/auth/logout", Auth::Bearer(&laptop), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        both_ingresses(&app, &laptop).await,
        (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        refresh(&app, &laptop_refresh).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        both_ingresses(&app, &phone).await,
        (StatusCode::OK, StatusCode::OK)
    );
}

/// ACC-002: deactivation (by any writer) ends tokens on v1, v2 and refresh — including
/// tokens issued before sessions existed — with no Communications integration
/// configured (ACC-003).
#[tokio::test]
async fn credential_changes_revoke_every_token_on_every_ingress() {
    let app = app_with_redis!();
    assert!(app.state.communications.is_none());
    let id = email_user(&app, "epoch@example.com", "active").await;
    let (session_access, session_refresh) = login(&app, "epoch@example.com").await;
    let legacy_access = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();
    let legacy_refresh = app
        .state
        .jwt_service
        .generate_refresh_token(id, "user")
        .unwrap();
    assert_eq!(both_ingresses(&app, &legacy_access).await.0, StatusCode::OK);
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Deactivation by any writer (here: direct SQL, as an operator tool would).
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE users SET status = 'inactive' WHERE id = '{id}'"
        ))
        .await
        .unwrap();
    for token in [&session_access, &legacy_access] {
        let (v1, v2) = both_ingresses(&app, token).await;
        assert!(v1.is_client_error() && v2.is_client_error(), "{v1} {v2}");
    }
    for token in [&session_refresh, &legacy_refresh] {
        assert_eq!(refresh(&app, token).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE user_id = '{id}' AND revoked_at IS NULL"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn sign_out_everywhere_and_per_session_revocation() {
    let app = app_with_redis!();
    let id = email_user(&app, "sessions@example.com", "active").await;
    let (a, _) = login(&app, "sessions@example.com").await;
    let (b, _) = login(&app, "sessions@example.com").await;

    let (s, v) = app.get("/api/v2/me/sessions", Auth::Bearer(&a)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let b_sid = claims(&app, &b).sid.unwrap();

    // Another user's session id is indistinguishable from an unknown one.
    let other = app.user("other").await;
    let (s, v) = app
        .post(
            format!("/api/v2/me/sessions/{b_sid}/revoke"),
            Auth::Bearer(&other.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(v["error"]["code"], "SESSION_NOT_FOUND");
    assert_eq!(both_ingresses(&app, &b).await.0, StatusCode::OK);

    // Revoke the second device from the first.
    let (s, v) = app
        .post(
            format!("/api/v2/me/sessions/{b_sid}/revoke"),
            Auth::Bearer(&a),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["revokedCount"], 1);
    assert_eq!(both_ingresses(&app, &b).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(both_ingresses(&app, &a).await.0, StatusCode::OK);

    // Everywhere, including tokens from before sessions existed.
    let legacy = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (s, v) = app
        .post(
            "/api/v2/me/sessions/revoke-all",
            Auth::Bearer(&a),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["revokedCount"], 1);
    for token in [&a, &legacy] {
        assert_eq!(
            both_ingresses(&app, token).await,
            (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
        );
    }
}

#[tokio::test]
async fn a_legacy_refresh_token_upgrades_once_into_a_session() {
    let app = app_with_redis!();
    let id = email_user(&app, "legacy@example.com", "active").await;
    let legacy = app
        .state
        .jwt_service
        .generate_refresh_token(id, "user")
        .unwrap();
    assert!(claims(&app, &legacy).sid.is_none());

    let (s, v) = refresh(&app, &legacy).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let upgraded = v["access_token"].as_str().unwrap();
    assert!(claims(&app, upgraded).sid.is_some());
    assert_eq!(both_ingresses(&app, upgraded).await.0, StatusCode::OK);
    // The legacy token cannot mint a second session.
    assert_eq!(refresh(&app, &legacy).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE user_id = '{id}' AND auth_method = 'LEGACY_REFRESH'"
        ))
        .await,
        1
    );
}

/// ACC-014: requesting a code answers the same for new, existing and deactivated
/// addresses, and a wrong code answers the same whether or not an account exists.
#[tokio::test]
async fn sign_in_answers_do_not_reveal_accounts() {
    let app = app_with_redis!();
    email_user(&app, "known@example.com", "active").await;
    email_user(&app, "gone@example.com", "inactive").await;

    let request = |email: &'static str| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/otp/request",
                Auth::None,
                json!({ "email": email }),
            )
            .await
        }
    };
    let known = request("known@example.com").await;
    assert_eq!(known.0, StatusCode::OK, "{}", known.1);
    assert_eq!(request("nobody@example.com").await, known);
    assert_eq!(request("gone@example.com").await, known);
    // Nothing is mailed to a deactivated account.
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM email_deliveries WHERE category = 'AUTH_VERIFICATION' \
              AND recipient_hash = encode(sha256(convert_to('gone@example.com', 'UTF8')), 'hex')"
        )
        .await,
        0
    );

    let verify = |email: &'static str| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/otp/verify",
                Auth::None,
                json!({ "email": email, "code": "999999" }),
            )
            .await
        }
    };
    let wrong = verify("known@example.com").await;
    let no_account = verify("never-requested@example.com").await;
    assert_eq!(wrong.0, StatusCode::BAD_REQUEST, "{}", wrong.1);
    assert_eq!(wrong.0, no_account.0);
    assert_eq!(wrong.1["error"]["code"], no_account.1["error"]["code"]);
    assert_eq!(
        wrong.1["error"]["message"],
        no_account.1["error"]["message"]
    );
    // No account is created by a failed verification.
    assert_eq!(
        app.scalar("SELECT count(*) FROM users WHERE email = 'never-requested@example.com'")
            .await,
        0
    );
}

/// The code as the user receives it: the line of the queued sign-in email that is exactly
/// six digits (N0-8: Redis holds only a salted hash of it).
async fn mailed_code(app: &TestApp, email: &str) -> String {
    let text: String = app
        .db
        .conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT text_body FROM email_deliveries WHERE category = 'AUTH_VERIFICATION' \
              AND recipient_hash = encode(sha256(convert_to(lower($1), 'UTF8')), 'hex') \
              ORDER BY created_at DESC LIMIT 1",
            [email.into()],
        ))
        .await
        .unwrap()
        .expect("a sign-in code was queued for mailing")
        .try_get("", "text_body")
        .unwrap();
    text.lines()
        .map(str::trim)
        .find(|l| l.len() == 6 && l.bytes().all(|b| b.is_ascii_digit()))
        .expect("the sign-in email carries the code on its own line")
        .to_string()
}

/// J-01 (passwordless): the mailed code signs in exactly once and records a LOGIN event;
/// the mailed email names the requesting browser.
#[tokio::test]
async fn the_mailed_code_signs_in_once() {
    let app = app_with_redis!();
    let email = format!(
        "mailed-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    );
    let id = email_user(&app, &email, "active").await;
    let (s, v) = app
        .post(
            "/api/v1/auth/otp/request",
            Auth::None,
            json!({ "email": email }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let code = mailed_code(&app, &email).await;
    let body = json!({ "email": email.to_uppercase(), "code": code });
    let (s, v) = app
        .post("/api/v1/auth/otp/verify", Auth::None, body.clone())
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["is_new_account"], false);
    assert_eq!(uuid(&v["user"]["id"]), id);
    let access = v["access_token"].as_str().unwrap().to_string();
    assert_eq!(
        both_ingresses(&app, &access).await,
        (StatusCode::OK, StatusCode::OK)
    );
    // Single use.
    let (s, _) = app.post("/api/v1/auth/otp/verify", Auth::None, body).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE user_id = '{id}' AND auth_method = 'EMAIL_OTP'"
        ))
        .await,
        1
    );
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' AND event_type = 'LOGIN' AND outcome = 'SUCCESS'"
        ))
        .await
            >= 1
    );
}

#[tokio::test]
async fn first_sign_in_creates_one_account_and_normalizes_email() {
    let app = app_with_redis!();
    let first = app.otp_sign_in("Mixed.Case@Example.COM").await;
    assert_eq!(first["is_new_account"], true);
    assert_eq!(first["user"]["status"], "active");
    let username = first["user"]["username"].as_str().unwrap();
    assert!(username.starts_with("user_"), "{username}");
    assert!(
        !username.contains("mixed"),
        "usernames never reveal the address"
    );
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM users WHERE email = 'mixed.case@example.com' \
              AND password_hash IS NULL AND email_verified_at IS NOT NULL"
        )
        .await,
        1
    );
    // A case variant signs in to the same account.
    let again = app.otp_sign_in("mixed.case@example.com").await;
    assert_eq!(again["is_new_account"], false);
    assert_eq!(again["user"]["id"], first["user"]["id"]);
    assert_eq!(
        app.scalar("SELECT count(*) FROM users WHERE lower(email) = 'mixed.case@example.com'")
            .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE category = 'AUTH_WELCOME' \
              AND idempotency_key = 'welcome:{}'",
            first["user"]["id"].as_str().unwrap()
        ))
        .await,
        1,
        "one welcome email for the new account"
    );
}

/// ACC-004: a Google identity never inherits an unverified account's password, never
/// replaces a different bound identity, and binds its subject.
#[tokio::test]
async fn google_identity_resolution_closes_the_pending_account_takeover() {
    let app = app_with_redis!();
    // Someone registered the victim's address with their own password (before sign-in
    // became passwordless) and never verified it.
    let pending = email_user(&app, "victim@example.com", "pending_verification").await;
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE users SET password_hash = 'legacy-hash' WHERE id = '{pending}'"
        ))
        .await
        .unwrap();
    let attacker_access = app
        .state
        .jwt_service
        .generate_access_token(pending, "user")
        .unwrap();
    let attacker_refresh = app
        .state
        .jwt_service
        .generate_refresh_token(pending, "user")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let google = |sub: &str, email: &str| GoogleIdentity {
        subject: sub.into(),
        email: email.into(),
        name: "Victim Person".into(),
    };
    let (user, event) =
        resolve_google_account(&app.state, &google("sub-victim", "victim@example.com"))
            .await
            .unwrap();
    assert_eq!(user.id, pending);
    assert_eq!(event, "GOOGLE_CLAIMED_UNVERIFIED_ACCOUNT");
    assert!(
        user.password_hash.is_none(),
        "attacker password must be discarded"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_state WHERE user_id = '{pending}' AND google_subject = 'sub-victim'"
        ))
        .await,
        1
    );
    // The attacker's earlier sign-in is over everywhere.
    assert_eq!(
        both_ingresses(&app, &attacker_access).await,
        (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        refresh(&app, &attacker_refresh).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Same subject again resolves the same account; a different subject is refused.
    let (again, event) =
        resolve_google_account(&app.state, &google("sub-victim", "victim@example.com"))
            .await
            .unwrap();
    assert_eq!((again.id, event), (pending, "GOOGLE_SIGN_IN"));
    assert!(
        resolve_google_account(&app.state, &google("sub-other", "victim@example.com"))
            .await
            .is_err()
    );

    // A verified account is linked, keeping its (legacy) credential.
    let active = email_user(&app, "linked@example.com", "active").await;
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE users SET password_hash = 'legacy-hash' WHERE id = '{active}'"
        ))
        .await
        .unwrap();
    let (linked, event) =
        resolve_google_account(&app.state, &google("sub-linked", "Linked@Example.com"))
            .await
            .unwrap();
    assert_eq!((linked.id, event), (active, "GOOGLE_LINKED"));
    assert!(linked.password_hash.is_some());

    // A new identity creates a Google account with a valid username.
    let (created, event) = resolve_google_account(
        &app.state,
        &GoogleIdentity {
            subject: "sub-new".into(),
            email: "new.person@example.com".into(),
            name: "  ".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(event, "GOOGLE_ACCOUNT_CREATED");
    assert_eq!(created.username, "new_person");
}

#[tokio::test]
async fn the_security_audit_is_append_only_and_admin_readable() {
    let app = app_with_redis!();
    let id = email_user(&app, "audited@example.com", "active").await;
    login(&app, "audited@example.com").await;

    let user = app.user("plain").await;
    let (s, _) = app
        .get("/api/v2/admin/audit-events", Auth::Bearer(&user.token))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let admin = app.admin("auditor").await;
    let (s, v) = app
        .get(
            format!("/api/v2/admin/audit-events?userId={id}&eventType=LOGIN"),
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["outcome"], "SUCCESS");
    assert!(!v.to_string().contains("audited@example.com"));

    let tamper = app
        .db
        .conn
        .execute_unprepared("UPDATE account_security_events SET outcome = 'FAILURE'")
        .await;
    assert!(tamper.is_err(), "audit rows must be immutable");
    let delete = app
        .db
        .conn
        .execute_unprepared("DELETE FROM account_security_events")
        .await;
    assert!(delete.is_err(), "audit rows must not be deletable");
}

/// Error paths for the session operations (anonymous callers are refused).
#[tokio::test]
async fn session_operations_require_a_signed_in_user() {
    let app = app_with_redis!();
    assert_eq!(
        app.get("/api/v2/me/sessions", Auth::None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.post("/api/v2/me/sessions/revoke-all", Auth::None, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.post(
            format!("/api/v2/me/sessions/{}/revoke", Uuid::new_v4()),
            Auth::None,
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

/// A3 / F1: every `400 VALIDATION_ERROR` lists `{field, code, message}` per problem under
/// `error.details` with the public field names, while `error.message` stays.
#[tokio::test]
async fn validation_errors_name_the_public_field_with_structured_details() {
    let app = app_with_redis!();
    let issues = |v: &Value| -> Vec<(String, String)> {
        v["error"]["details"]
            .as_array()
            .unwrap_or_else(|| panic!("details missing: {v}"))
            .iter()
            .map(|i| {
                assert!(i["message"].as_str().is_some_and(|m| !m.is_empty()), "{i}");
                (
                    i["field"].as_str().unwrap_or("<null>").to_string(),
                    i["code"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let pair = |f: &str, c: &str| (f.to_string(), c.to_string());

    // Several problems at once: one issue each, sorted by field.
    let (s, v) = app
        .post(
            "/api/v1/auth/otp/verify",
            Auth::None,
            json!({ "email": "not-an-email", "code": "12a456" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["success"], false);
    assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(
        issues(&v),
        vec![pair("code", "regex"), pair("email", "email")]
    );
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("code: The code must be 6 digits"), "{v}");
    assert!(message.contains("email: Invalid email address"), "{v}");
    assert!(
        !v.to_string().contains("12a456"),
        "the code is never echoed"
    );

    let (s, v) = app
        .post(
            "/api/v1/auth/otp/request",
            Auth::None,
            json!({ "email": "nope" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(issues(&v), vec![pair("email", "email")]);

    // Meeting creation: the title rule names `title`.
    let email = "validator@example.com";
    email_user(&app, email, "active").await;
    let (access, _) = login(&app, email).await;
    let (s, v) = app
        .post(
            "/api/v1/meetings",
            Auth::Bearer(&access),
            json!({
                "title": "", "start_time": "2030-01-25T14:00:00", "timezone": "UTC",
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(issues(&v), vec![pair("title", "length")]);
    assert_eq!(
        v["error"]["message"],
        "title: Title must be between 1 and 200 characters"
    );
}
