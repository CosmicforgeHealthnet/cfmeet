//! Blueprint v1.0 Section 2 — account security, against a real PostgreSQL and Redis.
//!
//! * ACC-001: typed tokens (refresh tokens never authenticate, access tokens never refresh)
//! * ACC-002/003: the revocation epoch applies on v1, v2 and refresh, without Communications
//! * ACC-007/017: sessions, per-session logout, rotation with reuse detection, legacy upgrade
//! * ACC-004: Google identity binding and the pending-account pre-hijack fix
//! * ACC-014 / DATA-002: uniform recovery answers, case-insensitive email identity
//! * SEC-013: append-only security audit and its administrator read
mod common;

use axum::http::StatusCode;
use backend::handlers::auth::{resolve_google_account, GoogleIdentity};
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
                format!("u_{}", &id.simple().to_string()[..10]).into(),
                email.into(),
                hash.into(),
                status.into(),
            ],
        ))
        .await
        .unwrap();
    id
}

async fn login(app: &TestApp, email: &str) -> (String, String) {
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": email, "password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (
        v["access_token"].as_str().unwrap().to_string(),
        v["refresh_token"].as_str().unwrap().to_string(),
    )
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
    password_user(&app, "alice.typed@example.com", "active").await;
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
    let id = password_user(&app, "rotate@example.com", "active").await;
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
    password_user(&app, "two.devices@example.com", "active").await;
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

/// ACC-002: password change, password reset, deactivation and logout-all end tokens on v1,
/// v2 and refresh — including tokens issued before sessions existed — with no
/// Communications integration configured (ACC-003).
#[tokio::test]
async fn credential_changes_revoke_every_token_on_every_ingress() {
    let app = app_with_redis!();
    assert!(app.state.communications.is_none());
    let id = password_user(&app, "epoch@example.com", "active").await;
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

    let (s, v) = app
        .post(
            "/api/v1/auth/change-password",
            Auth::Bearer(&session_access),
            json!({
                "current_password": PASSWORD,
                "new_password": "N3w!Passw0rd-long",
                "confirm_password": "N3w!Passw0rd-long",
            }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    for token in [&session_access, &legacy_access] {
        assert_eq!(
            both_ingresses(&app, token).await,
            (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED),
            "access token survived a password change"
        );
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

    // Deactivation by any writer (here: direct SQL, as an operator tool would) also ends tokens.
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": "epoch@example.com", "password": "N3w!Passw0rd-long" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let fresh = v["access_token"].as_str().unwrap().to_string();
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE users SET status = 'inactive' WHERE id = '{id}'"
        ))
        .await
        .unwrap();
    let (v1, v2) = both_ingresses(&app, &fresh).await;
    assert!(v1.is_client_error() && v2.is_client_error(), "{v1} {v2}");
}

#[tokio::test]
async fn sign_out_everywhere_and_per_session_revocation() {
    let app = app_with_redis!();
    let id = password_user(&app, "sessions@example.com", "active").await;
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
    let id = password_user(&app, "legacy@example.com", "active").await;
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

/// ACC-014: the recovery endpoints and login answer the same for unknown accounts.
#[tokio::test]
async fn recovery_and_login_answers_do_not_reveal_accounts() {
    let app = app_with_redis!();
    password_user(&app, "known@example.com", "active").await;

    let forgot = |email: &'static str| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/forgot-password",
                Auth::None,
                json!({ "email": email }),
            )
            .await
        }
    };
    let known = forgot("known@example.com").await;
    let unknown = forgot("nobody@example.com").await;
    assert_eq!(known, unknown);
    assert_eq!(known.0, StatusCode::OK);

    let resend = app
        .post(
            "/api/v1/auth/resend-verification",
            Auth::None,
            json!({ "email": "nobody@example.com" }),
        )
        .await;
    assert_eq!(resend, known);

    let wrong_password = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": "known@example.com", "password": "Wrong!Passw0rd" }),
        )
        .await;
    let no_account = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": "nobody@example.com", "password": "Wrong!Passw0rd" }),
        )
        .await;
    assert_eq!(wrong_password.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        wrong_password.1["error"]["message"],
        no_account.1["error"]["message"]
    );
    assert_eq!(wrong_password.0, no_account.0);

    let reset = |email: &'static str| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/reset-password",
                Auth::None,
                json!({
                    "email": email, "token": "123456",
                    "new_password": "N3w!Passw0rd-long", "confirm_password": "N3w!Passw0rd-long"
                }),
            )
            .await
        }
    };
    let a = reset("nobody@example.com").await;
    assert_eq!(a.0, StatusCode::BAD_REQUEST, "{}", a.1);
}

/// J-01 (final closure): the POSITIVE password-recovery path. A real reset code (as mailed,
/// read from the store the mailer reads) sets a new password; every session and token issued
/// before the reset — session and legacy, access and refresh, v1 and v2 — is refused; the new
/// password signs in, the old one does not; the code is single-use; a security event is kept.
#[tokio::test]
async fn password_reset_with_a_valid_code_ends_every_earlier_session() {
    let app = app_with_redis!();
    let email = format!(
        "reset-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    );
    let id = password_user(&app, &email, "active").await;
    let (session_access, session_refresh) = login(&app, &email).await;
    let (second_access, _) = login(&app, &email).await;
    let legacy_access = app
        .state
        .jwt_service
        .generate_access_token(id, "user")
        .unwrap();
    assert_eq!(
        both_ingresses(&app, &session_access).await,
        (StatusCode::OK, StatusCode::OK)
    );

    let (s, v) = app
        .post(
            "/api/v1/auth/forgot-password",
            Auth::None,
            json!({ "email": email }),
        )
        .await;
    assert!(s.is_success(), "{v}");
    // The code as the user receives it: from the queued reset email (N0-8: Redis holds only
    // a salted hash of it).
    let text: String = {
        use sea_orm::ConnectionTrait;
        app.db
            .conn
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT text_body FROM email_deliveries WHERE category = 'AUTH_PASSWORD_RESET' \
                  AND recipient_hash = encode(sha256(convert_to(lower($1), 'UTF8')), 'hex') \
                  ORDER BY created_at DESC LIMIT 1",
                [email.clone().into()],
            ))
            .await
            .unwrap()
            .expect("a reset code was issued for mailing")
            .try_get("", "text_body")
            .unwrap()
    };
    let code = text
        .split(|c: char| !c.is_ascii_digit())
        .find(|w| w.len() == 6)
        .expect("the reset email carries the code")
        .to_string();
    let new_password = "Reset!Passw0rd-long";
    let body = |token: &str| {
        json!({ "email": email, "token": token,
                "new_password": new_password, "confirm_password": new_password })
    };
    let (s, v) = app
        .post("/api/v1/auth/reset-password", Auth::None, body(&code))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    for token in [&session_access, &second_access, &legacy_access] {
        assert_eq!(
            both_ingresses(&app, token).await,
            (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED),
            "a token issued before the reset survived it"
        );
    }
    assert_eq!(
        refresh(&app, &session_refresh).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_sessions WHERE user_id = '{id}' AND revoked_at IS NULL"
        ))
        .await,
        0,
        "every session closed"
    );
    // The old password no longer signs in; the new one does (a fresh, working session).
    let (s, _) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": email, "password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": email, "password": new_password }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let fresh = v["access_token"].as_str().unwrap().to_string();
    assert_eq!(
        both_ingresses(&app, &fresh).await,
        (StatusCode::OK, StatusCode::OK)
    );
    // Single use: the same code cannot reset again.
    let (s, _) = app
        .post("/api/v1/auth/reset-password", Auth::None, body(&code))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' AND event_type = 'PASSWORD_RESET'"
        ))
        .await
            >= 1
    );
}

#[tokio::test]
async fn registration_normalizes_email_and_refuses_case_variants() {
    let app = app_with_redis!();
    let body = |email: &str, username: &str| {
        json!({
            "username": username, "email": email,
            "password": PASSWORD, "confirm_password": PASSWORD,
        })
    };
    let (s, v) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            body("Mixed.Case@Example.COM", "mixedcase"),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["email"], "mixed.case@example.com");
    // N0-9 / D-12: a case variant of a registered address gets the same answer as a new
    // registration (never 409); no second account is created and the owner is notified.
    let first = v.clone();
    let (s, again) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            body("mixed.case@example.com", "mixedcase2"),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{again}");
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&again), keys(&first), "same answer shape");
    assert_eq!(again["email"], first["email"]);
    assert_eq!(again["status"], first["status"]);
    assert_eq!(again["role"], first["role"]);
    assert_ne!(
        again["user_id"], first["user_id"],
        "the existing account id is never revealed"
    );
    assert!(again["user_id"].is_string());
    assert_eq!(
        app.scalar("SELECT count(*) FROM users WHERE lower(email) = 'mixed.case@example.com'")
            .await,
        1
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM users WHERE username = 'mixedcase2'")
            .await,
        0,
        "nothing is created for the second registration"
    );
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM email_deliveries WHERE category = 'NOTIFICATION' \
              AND idempotency_key LIKE 'register-existing:%'"
        )
        .await,
        1,
        "the account owner is told by email"
    );
    // A taken username is still refused (usernames are public).
    let (s, _) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            body("someone.else@example.com", "mixedcase"),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

/// ACC-004: a Google identity never inherits an unverified account's password, never
/// replaces a different bound identity, and binds its subject.
#[tokio::test]
async fn google_identity_resolution_closes_the_pending_account_takeover() {
    let app = app_with_redis!();
    // Someone registered the victim's address with their own password and never verified it.
    let pending = password_user(&app, "victim@example.com", "pending_verification").await;
    let (attacker_access, attacker_refresh) = login(&app, "victim@example.com").await;
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
    let (s, _) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({ "email": "victim@example.com", "password": PASSWORD }),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

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

    // A verified password account is linked, keeping its password.
    let active = password_user(&app, "linked@example.com", "active").await;
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
    let id = password_user(&app, "audited@example.com", "active").await;
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

/// Notification Service N0-10 (S-9): the password-changed notice is recorded in the same
/// transaction as the new password. If the notice cannot be recorded (injected database
/// failure), the change rolls back; once recording works, change and notice commit together.
#[tokio::test]
async fn a_password_change_and_its_security_notice_commit_together() {
    let app = app_with_redis!();
    let email = format!(
        "notice-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let id = password_user(&app, &email, "active").await;
    let notices = || {
        let app = &app;
        async move {
            app.scalar(&format!(
                "SELECT count(*) FROM email_deliveries WHERE category = 'SECURITY_PASSWORD_CHANGED' \
                  AND idempotency_key LIKE 'password-changed:{id}:%'"
            ))
            .await
        }
    };
    let change = |access: String| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/change-password",
                Auth::Bearer(&access),
                json!({
                    "current_password": PASSWORD,
                    "new_password": "N0tice!Passw0rd-long",
                    "confirm_password": "N0tice!Passw0rd-long",
                }),
            )
            .await
        }
    };
    // Failure injection: the email ledger refuses this category.
    app.db
        .conn
        .execute_unprepared(
            "CREATE FUNCTION n0_10_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
               IF NEW.category = 'SECURITY_PASSWORD_CHANGED' THEN RAISE EXCEPTION 'injected'; END IF; \
               RETURN NEW; END $$; \
             CREATE TRIGGER n0_10_refuse BEFORE INSERT ON email_deliveries \
               FOR EACH ROW EXECUTE FUNCTION n0_10_refuse();",
        )
        .await
        .unwrap();
    let (access, _) = login(&app, &email).await;
    let (s, _) = change(access).await;
    assert!(s.is_server_error(), "the change fails with its notice: {s}");
    assert_eq!(notices().await, 0);
    // The old password still works: nothing was changed without its notice.
    let (access, _) = login(&app, &email).await;

    app.db
        .conn
        .execute_unprepared(
            "DROP TRIGGER n0_10_refuse ON email_deliveries; DROP FUNCTION n0_10_refuse();",
        )
        .await
        .unwrap();
    let (s, v) = change(access).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        notices().await,
        1,
        "the notice is committed with the change"
    );
}

/// A3 / F1: a weak sign-up password is a field error on `password` (never `__all__`),
/// and every `400 VALIDATION_ERROR` lists `{field, code, message}` per problem under
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

    // Weak password (no special character, no upper case): field-level, on `password`.
    let (s, v) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            json!({
                "username": "weakling", "email": "weak@example.com",
                "password": "weakpass1", "confirm_password": "weakpass1",
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["success"], false);
    assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("password: Password must contain"),
        "{v}"
    );
    assert!(!v.to_string().contains("__all__"), "{v}");
    assert!(
        !v.to_string().contains("weakpass1"),
        "the password is never echoed"
    );
    assert_eq!(issues(&v), vec![pair("password", "password_strength")]);

    // Several problems at once: one issue each, sorted by field.
    let (s, v) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            json!({
                "username": "x", "email": "not-an-email",
                "password": PASSWORD, "confirm_password": "Different!1",
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        issues(&v),
        vec![
            pair("confirm_password", "passwords_mismatch"),
            pair("email", "email"),
            pair("username", "length"),
        ]
    );
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("confirm_password: Passwords do not match"),
        "{v}"
    );
    assert!(message.contains("email: Invalid email address"), "{v}");

    // A strong password is accepted.
    let (s, v) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            json!({
                "username": "strongone", "email": "strong@example.com",
                "password": PASSWORD, "confirm_password": PASSWORD,
            }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // Password reset uses the same field-level rule, on `new_password`.
    let (s, v) = app
        .post(
            "/api/v1/auth/reset-password",
            Auth::None,
            json!({
                "email": "strong@example.com", "token": "123456",
                "new_password": "ALLUPPER1!", "confirm_password": "ALLUPPER1!",
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(issues(&v), vec![pair("new_password", "password_strength")]);

    // Change password: reusing the current password is a `new_password` problem.
    let email = "changer@example.com";
    password_user(&app, email, "active").await;
    let (access, _) = login(&app, email).await;
    let (s, v) = app
        .post(
            "/api/v1/auth/change-password",
            Auth::Bearer(&access),
            json!({
                "current_password": PASSWORD,
                "new_password": PASSWORD,
                "confirm_password": PASSWORD,
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(issues(&v), vec![pair("new_password", "same_password")]);

    // Meeting creation: the title rule names `title`.
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
