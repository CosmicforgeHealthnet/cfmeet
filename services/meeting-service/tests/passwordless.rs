//! Passwordless sign-in improvements, against a real PostgreSQL and Redis:
//!
//! * code emails name the requesting browser; the magic link never signs in on GET;
//! * a sign-in from a device not seen recently triggers a security alert (not for a
//!   known device, a brand-new account or a test account);
//! * secure email change: step-up on the current mailbox, a code to the new one, other
//!   sessions end, the old address gets a 7-day undo link; the uniform answer for a taken
//!   address; one change per 7 days; support-assisted change by an administrator;
//! * fixed-code test accounts: no email, fixed code, never administrators;
//! * label-free `auth_*` counters.
mod common;

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

fn unique(tag: &str) -> String {
    format!(
        "{tag}-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

async fn sign_in_as(app: &TestApp, email: &str, user_agent: &str) -> Value {
    app.seed_sign_in_code(email, TEST_SIGN_IN_CODE).await;
    let body = json!({ "email": email, "code": TEST_SIGN_IN_CODE }).to_string();
    let (s, v) = app
        .call_with_headers(
            axum::http::Method::POST,
            "/api/v1/auth/otp/verify",
            &[("user-agent", user_agent)],
            body,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn alerts(app: &TestApp, email: &str) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM email_deliveries WHERE idempotency_key LIKE 'new-sign-in:%' \
          AND recipient_hash = encode(sha256(convert_to('{email}', 'UTF8')), 'hex')"
    ))
    .await
}

async fn latest_text(app: &TestApp, key_prefix: &str, email: &str) -> String {
    app.db
        .conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT text_body FROM email_deliveries WHERE idempotency_key LIKE $1 \
              AND recipient_hash = encode(sha256(convert_to(lower($2), 'UTF8')), 'hex') \
              ORDER BY created_at DESC LIMIT 1",
            [format!("{key_prefix}%").into(), email.into()],
        ))
        .await
        .unwrap()
        .expect("email queued")
        .try_get("", "text_body")
        .unwrap()
}

fn six_digit_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| l.len() == 6 && l.bytes().all(|b| b.is_ascii_digit()))
        .expect("a code line")
        .to_string()
}

#[tokio::test]
async fn code_emails_name_the_browser_and_the_link_never_signs_in_on_get() {
    let app = app_with_redis!();
    let email = unique("details");
    let (s, v) = app
        .call_with_headers(
            axum::http::Method::POST,
            "/api/v1/auth/otp/request",
            &[("user-agent", "Mozilla/5.0 TestBrowser/1.0")],
            json!({ "email": email }).to_string(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let text = latest_text(&app, "otp:signin:", &email).await;
    assert!(text.contains("TestBrowser/1.0"), "{text}");
    assert!(text.contains("(UTC)"), "{text}");
    assert!(text.contains("10 minutes"), "{text}");
    // The link is a page of the web app that POSTs; the API endpoint has no GET.
    let (s, _) = app
        .get("/api/v1/auth/otp/verify-link?token=abc", Auth::None)
        .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn new_devices_trigger_a_sign_in_alert() {
    let app = app_with_redis!();
    let email = unique("alert");
    // A brand-new account: welcome email, no alert.
    let first = sign_in_as(&app, &email, "Laptop Browser").await;
    assert_eq!(first["is_new_account"], true);
    assert_eq!(alerts(&app, &email).await, 0);
    // The same browser again: no alert.
    sign_in_as(&app, &email, "Laptop Browser").await;
    assert_eq!(alerts(&app, &email).await, 0);
    // A browser not seen on this account: one alert, naming it.
    sign_in_as(&app, &email, "Unknown Phone Browser").await;
    assert_eq!(alerts(&app, &email).await, 1);
    let text = latest_text(&app, "new-sign-in:", &email).await;
    assert!(text.contains("Unknown Phone Browser"), "{text}");
    assert!(text.contains("/settings/sessions"), "{text}");
}

#[tokio::test]
async fn email_change_proves_both_mailboxes_and_can_be_undone() {
    let app = app_with_redis!();
    let old = unique("old");
    let new = unique("new");
    let signed = app.otp_sign_in(&old).await;
    let id = uuid(&signed["user"]["id"]);
    let access = signed["access_token"].as_str().unwrap().to_string();
    let (other_access, _) = app.otp_tokens(&old).await;

    // Without a CHANGE_EMAIL step-up token nothing happens.
    let (s, v) = app
        .post(
            "/api/v2/me/email/change/request",
            Auth::Bearer(&access),
            json!({ "newEmail": new, "stepUpToken": "nope" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "REAUTHENTICATION_REQUIRED");

    // Step-up, then a code to the new address.
    let token = app.step_up_token(&access, id, "CHANGE_EMAIL").await;
    let (s, v) = app
        .post(
            "/api/v2/me/email/change/request",
            Auth::Bearer(&access),
            json!({ "newEmail": new, "stepUpToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let code_new = six_digit_line(&latest_text(&app, "otp:emailchange:", &new).await);
    let (s, _) = app
        .post(
            "/api/v2/me/email/change/verify",
            Auth::Bearer(&access),
            json!({ "newEmail": new, "code": if code_new == "000000" { "111111" } else { "000000" } }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = app
        .post(
            "/api/v2/me/email/change/verify",
            Auth::Bearer(&access),
            json!({ "newEmail": new, "code": code_new }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["email"], new.as_str());

    // This session stays; the other one ended.
    assert_eq!(
        app.get("/api/v2/me/sessions", Auth::Bearer(&access)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        app.get("/api/v2/me/sessions", Auth::Bearer(&other_access))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // The old address got the notice with an undo link; the new one a confirmation.
    let notice = latest_text(&app, "email-changed:old:", &old).await;
    assert!(notice.contains("n***@example.com") || notice.contains("***@example.com"));
    let undo: String = notice
        .split_whitespace()
        .find_map(|w| w.split("token=").nth(1))
        .expect("undo link")
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    assert_eq!(undo.len(), 64);
    latest_text(&app, "email-changed:new:", &new).await;
    // The new address signs in to the same account; the old one would create a new one.
    let again = app.otp_sign_in(&new).await;
    assert_eq!(uuid(&again["user"]["id"]), id);

    // A second change within 7 days is refused.
    let token = app.step_up_token(&access, id, "CHANGE_EMAIL").await;
    let (s, _) = app
        .post(
            "/api/v2/me/email/change/request",
            Auth::Bearer(&access),
            json!({ "newEmail": unique("third"), "stepUpToken": token }),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);

    // Undo: the old address is back and every session ended; single use.
    let (s, v) = app
        .post(
            "/api/v1/auth/email-change/revert",
            Auth::None,
            json!({ "token": undo }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM users WHERE id = '{id}' AND email = '{old}'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.get("/api/v2/me/sessions", Auth::Bearer(&access)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (s, _) = app
        .post(
            "/api/v1/auth/email-change/revert",
            Auth::None,
            json!({ "token": undo }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "single use");
    for kind in ["EMAIL_CHANGED", "EMAIL_CHANGE_REVERTED"] {
        assert_eq!(
            app.scalar(&format!(
                "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' AND event_type = '{kind}'"
            ))
            .await,
            1,
            "{kind}"
        );
    }
}

#[tokio::test]
async fn an_expired_undo_link_does_nothing() {
    let app = app_with_redis!();
    let user = app.user("expired").await;
    let token = "ab".repeat(32);
    let digest = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(token.as_bytes()))
    };
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO email_change_reverts (id, user_id, old_email, new_email, token_digest, created_at, expires_at) \
             VALUES ('{}', '{}', 'before@example.com', '{}', '{digest}', now() - interval '8 days', now() - interval '1 day')",
            Uuid::new_v4(),
            user.id,
            user.email
        ))
        .await
        .unwrap();
    let (s, _) = app
        .post(
            "/api/v1/auth/email-change/revert",
            Auth::None,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM users WHERE id = '{}' AND email = '{}'",
            user.id, user.email
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn a_taken_address_gets_the_same_answer_and_a_notice_instead_of_a_code() {
    let app = app_with_redis!();
    let me = unique("mover");
    let taken = app.user("owner").await;
    let signed = app.otp_sign_in(&me).await;
    let id = uuid(&signed["user"]["id"]);
    let access = signed["access_token"].as_str().unwrap().to_string();

    let token = app.step_up_token(&access, id, "CHANGE_EMAIL").await;
    let (s_taken, v_taken) = app
        .post(
            "/api/v2/me/email/change/request",
            Auth::Bearer(&access),
            json!({ "newEmail": taken.email, "stepUpToken": token }),
        )
        .await;
    let token = app.step_up_token(&access, id, "CHANGE_EMAIL").await;
    let (s_free, v_free) = app
        .post(
            "/api/v2/me/email/change/request",
            Auth::Bearer(&access),
            json!({ "newEmail": unique("free"), "stepUpToken": token }),
        )
        .await;
    assert_eq!((s_taken, &v_taken), (s_free, &v_free));
    assert_eq!(s_taken, StatusCode::OK);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE idempotency_key LIKE 'email-in-use:%' \
              AND recipient_hash = encode(sha256(convert_to('{}', 'UTF8')), 'hex')",
            taken.email
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE idempotency_key LIKE 'otp:emailchange:%' \
              AND recipient_hash = encode(sha256(convert_to('{}', 'UTF8')), 'hex')",
            taken.email
        ))
        .await,
        0,
        "no code is ever sent to an address that has an account"
    );
}

#[tokio::test]
async fn support_changes_an_email_with_an_audited_admin_call() {
    let app = app_with_redis!();
    let user = app.user("lostmail").await;
    let (access, _) = app.otp_tokens(&user.email).await;
    let admin = app.admin("support").await;
    let new = unique("recovered");
    let body = json!({ "new_email": new, "reason": "Verified by video call and ID check" });

    let (s, _) = app
        .patch(
            format!("/api/v1/admin/users/{}/email", user.id),
            Auth::Bearer(&user.token),
            body.clone(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "admins only");
    let (s, v) = app
        .patch(
            format!("/api/v1/admin/users/{}/email", user.id),
            Auth::Bearer(&admin.token),
            json!({ "new_email": new, "reason": "short" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a reason is required: {v}");
    let (s, v) = app
        .patch(
            format!("/api/v1/admin/users/{}/email", user.id),
            Auth::Bearer(&admin.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["email"], new.as_str());
    // Every session ended; both addresses were told; the change is audited.
    assert_eq!(
        app.get("/api/v2/me/sessions", Auth::Bearer(&access)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let notice = latest_text(&app, "email-changed:old:", &user.email).await;
    assert!(notice.contains("CF Meet support") && !notice.contains("token="));
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{}' \
             AND event_type = 'EMAIL_CHANGED_BY_ADMIN' AND actor_kind = 'ADMIN' AND actor_user_id = '{}'",
            user.id, admin.id
        ))
        .await,
        1
    );
    // An address of another account is refused.
    let (s, _) = app
        .patch(
            format!("/api/v1/admin/users/{}/email", user.id),
            Auth::Bearer(&admin.token),
            json!({ "new_email": admin.email, "reason": "Verified by video call and ID check" }),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn test_accounts_use_a_fixed_code_get_no_email_and_cannot_be_admins() {
    let app = app_with_redis!();
    let review = unique("review");
    let app = app.with_auth_env(&[
        ("APP_ENV", "test"),
        ("AUTH_TEST_ACCOUNTS", &format!("{review}:246810")),
    ]);
    let (s, _) = app
        .post(
            "/api/v1/auth/otp/request",
            Auth::None,
            json!({ "email": review }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE recipient_hash = encode(sha256(convert_to('{review}', 'UTF8')), 'hex')"
        ))
        .await,
        0,
        "no email for a test account"
    );
    let (s, _) = app
        .post(
            "/api/v1/auth/otp/verify",
            Auth::None,
            json!({ "email": review, "code": "246811" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = app
        .post(
            "/api/v1/auth/otp/verify",
            Auth::None,
            json!({ "email": review, "code": "246810" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = uuid(&v["user"]["id"]);
    // Reusable (fixed), audited as a test account.
    let (s, _) = app
        .post(
            "/api/v1/auth/otp/verify",
            Auth::None,
            json!({ "email": review, "code": "246810" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE user_id = '{id}' \
             AND event_type = 'LOGIN' AND detail->>'test_account' = 'true'"
        ))
        .await,
        2
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE recipient_hash = encode(sha256(convert_to('{review}', 'UTF8')), 'hex')"
        ))
        .await,
        0,
        "no welcome or alert email either"
    );
    let admin = app.admin("granter").await;
    let (s, _) = app
        .patch(
            format!("/api/v1/admin/users/{id}"),
            Auth::Bearer(&admin.token),
            json!({ "role": "admin" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a test account is never an admin");
}

#[tokio::test]
async fn auth_counters_move() {
    use backend::services::ops_metrics::OPS_METRICS as OM;
    use std::sync::atomic::Ordering::Relaxed;
    let app = app_with_redis!();
    let before = (
        OM.auth_otp_requested.load(Relaxed),
        OM.auth_otp_verified.load(Relaxed),
        OM.auth_otp_wrong.load(Relaxed),
        OM.auth_account_created.load(Relaxed),
    );
    let email = unique("metrics");
    app.post(
        "/api/v1/auth/otp/request",
        Auth::None,
        json!({ "email": email }),
    )
    .await;
    app.post(
        "/api/v1/auth/otp/verify",
        Auth::None,
        json!({ "email": email, "code": "999999" }),
    )
    .await;
    app.otp_sign_in(&email).await;
    // Counters are process-wide (tests run in parallel): only "at least" holds.
    assert!(OM.auth_otp_requested.load(Relaxed) > before.0);
    assert!(OM.auth_otp_verified.load(Relaxed) > before.1);
    assert!(OM.auth_otp_wrong.load(Relaxed) > before.2);
    assert!(OM.auth_account_created.load(Relaxed) > before.3);
    let snapshot = OM.snapshot().to_string();
    assert!(snapshot.contains("auth_code_email_slow"), "{snapshot}");
    assert!(!snapshot.contains(&email));
}
