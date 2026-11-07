//! Blueprint v1.0 Section 16 (INV-002): the legacy `/api/v1` account, API-key, webhook
//! and admin operations that no automated test referenced, against a real PostgreSQL
//! and Redis.
//!
//! * API keys (`/api/v1/api-keys*`): issue once, masked reads, owner scoping, revoke once.
//! * Webhooks (`/api/v1/webhooks*`, PLT-003 / DATA-007): owner-scoped CRUD that says on
//!   every answer that nothing is delivered (`Deprecation` / `Sunset` / `Link` headers,
//!   `delivery: NOT_DELIVERED`, a `notice` pointing to Developer Platform webhooks).
//! * Email verification and the reset-code probe (`verify-email`, `verify-reset-token`).
//! * Google sign-in start, callback refusals and the one-time exchange code (no call to
//!   Google is ever made: every path tested fails before the token exchange).
//! * Admin user, meeting and log reads/writes require the admin role.
//!
//! No email is sent: codes are seeded straight into the OTP store and the email worker
//! does not run in tests.
mod common;

use axum::{
    body::Body,
    http::{header, HeaderMap, Method, Request, StatusCode},
};
use backend::services::otp::OtpType;
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
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

/// Raw request that also returns the response headers.
async fn raw(
    app: &TestApp,
    method: Method,
    uri: &str,
    bearer: Option<&str>,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }))
    };
    (status, headers, value)
}

fn header_str<'a>(h: &'a HeaderMap, name: &str) -> &'a str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

async fn insert_user(app: &TestApp, email: &str, status: &str) -> Uuid {
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

fn unique_email(tag: &str) -> String {
    format!(
        "{tag}-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_api_keys_issue_once_mask_reads_scope_to_owner_and_revoke_once() {
    let app = app_with_redis!();
    let owner = app.user("keyowner").await;
    let other = app.user("keyother").await;

    // Every operation requires a bearer token.
    for (method, uri) in [
        (Method::GET, "/api/v1/api-keys".to_string()),
        (Method::POST, "/api/v1/api-keys".to_string()),
        (Method::GET, format!("/api/v1/api-keys/{}", Uuid::new_v4())),
        (
            Method::DELETE,
            format!("/api/v1/api-keys/{}", Uuid::new_v4()),
        ),
    ] {
        let (s, _, v) = raw(&app, method.clone(), &uri, None, Some(json!({})), None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}: {v}");
    }

    let (s, v) = app
        .post("/api/v1/api-keys", Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let key_id = uuid(&v["data"]["id"]);
    let secret = v["data"]["api_key"].as_str().unwrap().to_string();
    assert_eq!(secret.len(), 64);
    assert!(secret.chars().all(|c| c.is_ascii_alphanumeric()));
    assert_eq!(v["data"]["status"], "active");
    assert_eq!(v["data"]["usage_limit"], 100);
    assert_eq!(v["data"]["used_count"], 0);

    // Reads never return the secret again: only the last four characters.
    let (s, v) = app
        .get("/api/v1/api-keys", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let list = v["data"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(uuid(&list[0]["id"]), key_id);
    let masked = list[0]["api_key_masked"].as_str().unwrap();
    assert!(masked.ends_with(&secret[60..]), "{masked}");
    assert!(!v.to_string().contains(&secret));

    let (s, v) = app
        .get(
            format!("/api/v1/api-keys/{key_id}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["api_key_masked"], masked);
    assert!(!v.to_string().contains(&secret));

    // Another user can neither see nor revoke it (404, not 403: no existence oracle).
    let (s, v) = app
        .get("/api/v1/api-keys", Auth::Bearer(&other.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["data"].as_array().unwrap().is_empty());
    let (s, _) = app
        .get(
            format!("/api/v1/api-keys/{key_id}"),
            Auth::Bearer(&other.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .delete(
            format!("/api/v1/api-keys/{key_id}"),
            Auth::Bearer(&other.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Revoke once; a second revoke is a conflict and the key reads as revoked.
    let (s, v) = app
        .delete(
            format!("/api/v1/api-keys/{key_id}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = app
        .delete(
            format!("/api/v1/api-keys/{key_id}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = app
        .get(
            format!("/api/v1/api-keys/{key_id}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"]["status"], "revoked");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM api_keys WHERE id = '{key_id}' AND status::text = 'revoked'"
        ))
        .await,
        1
    );
}

// ---------------------------------------------------------------------------
// Webhooks (PLT-003 / DATA-007: registered, never delivered, and saying so)
// ---------------------------------------------------------------------------

fn assert_deprecated(headers: &HeaderMap, context: &str) {
    assert!(
        !header_str(headers, "deprecation").is_empty(),
        "{context}: missing Deprecation header"
    );
    assert!(
        !header_str(headers, "sunset").is_empty(),
        "{context}: missing Sunset header"
    );
    assert!(
        header_str(headers, "link").contains("rel=\"deprecation\""),
        "{context}: Link header must carry rel=\"deprecation\" ({:?})",
        headers.get("link")
    );
}

fn assert_notice(wrapper: &Value, context: &str) {
    let notice = wrapper["notice"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        notice.contains("deliver"),
        "{context}: notice must say nothing is delivered: {wrapper}"
    );
    assert!(
        notice.contains("developer platform"),
        "{context}: notice must point to Developer Platform webhooks: {wrapper}"
    );
}

#[tokio::test]
async fn legacy_webhooks_are_owner_scoped_and_every_answer_says_nothing_is_delivered() {
    let app = app_with_redis!();
    let owner = app.user("hookowner").await;
    let other = app.user("hookother").await;

    let (s, h, v) = raw(&app, Method::GET, "/api/v1/webhooks", None, None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
    assert_deprecated(&h, "GET /api/v1/webhooks without a token");

    let (s, h, v) = raw(
        &app,
        Method::POST,
        "/api/v1/webhooks",
        Some(&owner.token),
        Some(
            json!({ "event_type": "not_an_event", "endpoint_url": "https://hooks.example.com/x" }),
        ),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    assert_deprecated(&h, "invalid create");

    let (s, h, v) = raw(
        &app,
        Method::POST,
        "/api/v1/webhooks",
        Some(&owner.token),
        Some(json!({ "event_type": "meeting_start", "endpoint_url": "https://hooks.example.com/meetings" })),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_deprecated(&h, "create");
    assert_notice(&v, "create");
    assert_eq!(v["data"]["delivery"], "NOT_DELIVERED");
    assert_eq!(v["data"]["event_type"], "meeting_start");
    assert_eq!(v["data"]["status"], "active");
    assert!(!v["data"]["secret"].as_str().unwrap_or_default().is_empty());
    let hook = uuid(&v["data"]["id"]);
    let secret = v["data"]["secret"].as_str().unwrap().to_string();

    let (s, h, v) = raw(
        &app,
        Method::GET,
        "/api/v1/webhooks",
        Some(&owner.token),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_deprecated(&h, "list");
    assert_notice(&v, "list");
    let list = v["data"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(uuid(&list[0]["id"]), hook);
    assert_eq!(list[0]["delivery"], "NOT_DELIVERED");
    assert!(
        !v.to_string().contains(&secret),
        "the secret is returned only on create"
    );

    let item = format!("/api/v1/webhooks/{hook}");
    let (s, h, v) = raw(
        &app,
        Method::PATCH,
        &item,
        Some(&owner.token),
        Some(json!({ "status": "inactive", "endpoint_url": "https://hooks.example.com/v2" })),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_deprecated(&h, "update");
    assert_notice(&v, "update");
    assert_eq!(v["data"]["status"], "inactive");
    assert_eq!(v["data"]["endpoint_url"], "https://hooks.example.com/v2");
    assert_eq!(v["data"]["delivery"], "NOT_DELIVERED");

    let (s, h, _) = raw(
        &app,
        Method::PATCH,
        &item,
        Some(&owner.token),
        Some(json!({ "status": "paused" })),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_deprecated(&h, "invalid update");

    // Another user's token: invisible, not modifiable, not deletable.
    let (s, _, v) = raw(
        &app,
        Method::GET,
        "/api/v1/webhooks",
        Some(&other.token),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["data"].as_array().unwrap().is_empty());
    let (s, h, _) = raw(
        &app,
        Method::PATCH,
        &item,
        Some(&other.token),
        Some(json!({ "status": "active" })),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_deprecated(&h, "foreign update");
    let (s, _, _) = raw(&app, Method::DELETE, &item, Some(&other.token), None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, h, v) = raw(&app, Method::DELETE, &item, Some(&owner.token), None, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_deprecated(&h, "delete");
    let (s, _, _) = raw(&app, Method::DELETE, &item, Some(&owner.token), None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM webhooks WHERE id = '{hook}'"
        ))
        .await,
        0
    );
}

// ---------------------------------------------------------------------------
// Email verification and the reset-code probe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn verify_email_activates_a_pending_account_once_without_revealing_accounts() {
    let app = app_with_redis!();
    let email = unique_email("verify");
    let id = insert_user(&app, &email, "pending_verification").await;
    app.state
        .otp_service
        .store_code(&email, "246810", OtpType::Verification)
        .await
        .unwrap();

    let verify = |email: String, token: &'static str| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/verify-email",
                Auth::None,
                json!({ "email": email, "token": token }),
            )
            .await
        }
    };
    let wrong = verify(email.clone(), "135790").await;
    assert_eq!(wrong.0, StatusCode::BAD_REQUEST, "{}", wrong.1);
    let unknown = verify(unique_email("nobody"), "246810").await;
    assert_eq!(unknown.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        wrong.1["error"]["message"], unknown.1["error"]["message"],
        "an unknown account answers like a wrong code"
    );
    let malformed = verify(email.clone(), "12").await;
    assert_eq!(malformed.0, StatusCode::BAD_REQUEST);

    let (s, v) = verify(email.clone(), "246810").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM users WHERE id = '{id}' AND status::text = 'active' AND email_verified_at IS NOT NULL"
        ))
        .await,
        1
    );
    // Single use: the code is gone and an active account cannot be "verified" again.
    let replay = verify(email.clone(), "246810").await;
    assert_eq!(replay.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM account_security_events WHERE (user_id = '{id}' OR actor_user_id = '{id}') AND event_type = 'EMAIL_VERIFIED'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn verify_reset_token_reports_a_pending_reset_without_consuming_it() {
    let app = app_with_redis!();
    let email = unique_email("resetprobe");
    insert_user(&app, &email, "active").await;
    let probe = |email: String| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/verify-reset-token",
                Auth::None,
                json!({ "email": email, "token": "123456" }),
            )
            .await
        }
    };

    let none = probe(email.clone()).await;
    assert_eq!(none.0, StatusCode::BAD_REQUEST, "{}", none.1);
    let unknown = probe(unique_email("nobody")).await;
    assert_eq!(unknown.0, StatusCode::BAD_REQUEST);
    assert_eq!(none.1["error"]["message"], unknown.1["error"]["message"]);

    app.state
        .otp_service
        .store_code(&email, "975310", OtpType::PasswordReset)
        .await
        .unwrap();
    for _ in 0..2 {
        let (s, v) = probe(email.clone()).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v["valid"], true);
    }
    // The probe never compares or consumes the code.
    assert!(app
        .state
        .otp_service
        .has_code(&email, OtpType::PasswordReset)
        .await
        .unwrap());

    let (s, _) = app
        .post(
            "/api/v1/auth/verify-reset-token",
            Auth::None,
            json!({ "email": "not-an-email", "token": "123456" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Google sign-in (no request ever reaches Google in these paths)
// ---------------------------------------------------------------------------

fn query_param(location: &str, name: &str) -> Option<String> {
    let query = location.split_once('?')?.1;
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| urlencoding::decode(v).unwrap().into_owned())
}

#[tokio::test]
async fn google_sign_in_start_binds_the_browser_and_the_callback_refuses_bad_state() {
    let app = app_with_redis!();
    let (s, h, _) = raw(
        &app,
        Method::GET,
        "/api/v1/auth/oauth/google",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let location = header_str(&h, "location").to_string();
    assert!(
        location.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
        "{location}"
    );
    assert_eq!(
        query_param(&location, "response_type").as_deref(),
        Some("code")
    );
    let state_value = query_param(&location, "state").expect("state");
    let nonce = query_param(&location, "nonce").expect("nonce");
    assert!(state_value.len() >= 32 && nonce.len() >= 32 && state_value != nonce);
    let cookie = header_str(&h, "set-cookie").to_string();
    assert!(cookie.starts_with("cfm_oauth_binding="), "{cookie}");
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"),
        "{cookie}"
    );
    assert!(cookie.contains("Path=/api/v1/auth/oauth"), "{cookie}");
    assert_eq!(header_str(&h, "cache-control"), "no-store");
    let binding = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("cfm_oauth_binding=")
        .to_string();
    let stored = app
        .state
        .redis
        .get(&format!("oauth:state:{state_value}"))
        .await
        .unwrap()
        .expect("state stored server-side");
    assert!(stored.contains(&nonce));
    assert!(
        !stored.contains(&binding),
        "only a digest of the binding is stored"
    );

    let frontend = format!("{}/google/callback?error=", app.state.app_url);
    let callback = |query: String, cookie: Option<String>| {
        let app = &app;
        async move {
            let uri = format!("/api/v1/auth/oauth/google/callback{query}");
            let (s, h, _) = raw(app, Method::GET, &uri, None, None, cookie.as_deref()).await;
            assert_eq!(s, StatusCode::SEE_OTHER, "{uri}");
            header_str(&h, "location").to_string()
        }
    };
    assert_eq!(
        callback("?error=access_denied".into(), None).await,
        format!("{frontend}oauth_cancelled")
    );
    assert_eq!(
        callback("?state=only".into(), None).await,
        format!("{frontend}oauth_invalid_request")
    );
    assert_eq!(
        callback(format!("?code=c&state={}", Uuid::new_v4().simple()), None).await,
        format!("{frontend}oauth_invalid_state")
    );

    if app.state.auth_policy.google_browser_binding {
        // A different browser cannot finish the sign-in, and the attempt burns the state:
        // even the right browser is then refused (single use). Both fail before any call
        // to Google's token endpoint.
        assert_eq!(
            callback(
                format!("?code=c&state={state_value}"),
                Some("cfm_oauth_binding=someone-else".into())
            )
            .await,
            format!("{frontend}oauth_invalid_state")
        );
        assert_eq!(
            callback(
                format!("?code=c&state={state_value}"),
                Some(format!("cfm_oauth_binding={binding}"))
            )
            .await,
            format!("{frontend}oauth_invalid_state")
        );
        assert!(app
            .state
            .redis
            .get(&format!("oauth:state:{state_value}"))
            .await
            .unwrap()
            .is_none());
    }
}

#[tokio::test]
async fn google_exchange_code_signs_in_once() {
    let app = app_with_redis!();
    let user = app.user("googler").await;
    let code = format!("x{}", Uuid::new_v4().simple());
    let bundle = json!({
        "access_token": "access-token-from-callback",
        "refresh_token": "refresh-token-from-callback",
        "user_id": user.id,
        "username": "googler",
        "role": "user",
        "status": "active",
    });
    app.state
        .redis
        .set(
            &format!(
                "oauth:exchange:{}",
                hex::encode(Sha256::digest(code.as_bytes()))
            ),
            &bundle.to_string(),
            Some(60),
        )
        .await
        .unwrap();

    let exchange = |code: String| {
        let app = &app;
        async move {
            app.post(
                "/api/v1/auth/oauth/google/exchange",
                Auth::None,
                json!({ "exchange_code": code }),
            )
            .await
        }
    };
    let (s, v) = exchange(code.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["access_token"], "access-token-from-callback");
    assert_eq!(v["refresh_token"], "refresh-token-from-callback");
    assert_eq!(uuid(&v["user"]["id"]), user.id);
    assert_eq!(v["user"]["status"], "active");

    let (s, _) = exchange(code.clone()).await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "the exchange code is single use"
    );
    let (s, _) = exchange(format!("y{}", Uuid::new_v4().simple())).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = exchange("short".into()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_user_meeting_and_log_operations_require_the_admin_role() {
    let app = app_with_redis!();
    let plain = app.user("plainuser").await;
    let admin = app.admin("legacyadmin").await;
    let target = app.user("target").await;

    let meeting = Uuid::new_v4();
    let code = format!("A{}", &meeting.simple().to_string()[..7])
        .to_uppercase()
        .replace(['0', '1', 'O', 'I'], "Q");
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO meetings (id, meeting_identifier, user_id, host_id, title, is_private, start_time, end_time, status) \
             VALUES ('{meeting}', '{code}', '{h}', '{h}', 'admin-visible', false, now() - interval '5 minutes', now() + interval '1 hour', 'ongoing'); \
             INSERT INTO session_logs (meeting_id, event_type, event_time) VALUES \
             ('{meeting}', 'meeting_start', now() - interval '4 minutes'), \
             ('{meeting}', 'participant_join', now() - interval '3 minutes')",
            h = target.id
        ))
        .await
        .unwrap();

    let user_uri = format!("/api/v1/admin/users/{}", target.id);
    for (method, uri) in [
        (
            Method::GET,
            format!("/api/v1/admin/meetings?host_id={}", target.id),
        ),
        (
            Method::GET,
            format!("/api/v1/admin/logs?meeting_id={meeting}"),
        ),
        (Method::PATCH, user_uri.clone()),
    ] {
        let (s, v) = app
            .call(
                method.clone(),
                &uri,
                Auth::Bearer(&plain.token),
                Some(json!({ "status": "inactive" })),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {uri}: {v}");
        let (s, _) = app
            .call(
                method.clone(),
                &uri,
                Auth::None,
                Some(json!({ "status": "inactive" })),
            )
            .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }

    let (s, v) = app
        .get(
            format!("/api/v1/admin/meetings?host_id={}", target.id),
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let rows = v["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(uuid(&rows[0]["id"]), meeting);
    assert_eq!(rows[0]["status"], "ongoing");
    assert_eq!(v["meta"]["total"], 1);
    let (s, _) = app
        .get(
            "/api/v1/admin/meetings?status=bogus",
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, v) = app
        .get(
            format!("/api/v1/admin/logs?meeting_id={meeting}"),
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let logs = v["data"].as_array().unwrap();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|l| uuid(&l["meeting_id"]) == meeting));
    assert_eq!(logs[0]["event_type"], "participant_join", "newest first");
    let (s, v) = app
        .get(
            format!("/api/v1/admin/logs?meeting_id={meeting}&event_type=meeting_start"),
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    let (s, _) = app
        .get(
            "/api/v1/admin/logs?event_type=bogus",
            Auth::Bearer(&admin.token),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // An admin cannot change their own role; unknown users are 404.
    let (s, _) = app
        .patch(
            format!("/api/v1/admin/users/{}", admin.id),
            Auth::Bearer(&admin.token),
            json!({ "role": "user" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .patch(
            format!("/api/v1/admin/users/{}", Uuid::new_v4()),
            Auth::Bearer(&admin.token),
            json!({ "status": "inactive" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .patch(
            &user_uri,
            Auth::Bearer(&admin.token),
            json!({ "role": "superuser" }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Deactivating an account takes effect on its next request.
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&target.token))
            .await
            .0,
        StatusCode::OK
    );
    let (s, v) = app
        .patch(
            &user_uri,
            Auth::Bearer(&admin.token),
            json!({ "status": "inactive" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["data"]["status"], "inactive");
    assert_eq!(
        app.get("/api/v1/users/me", Auth::Bearer(&target.token))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

// ---------------------------------------------------------------------------
// Service root
// ---------------------------------------------------------------------------

#[tokio::test]
async fn service_root_describes_the_api_without_authentication() {
    let app = match TestApp::new().await {
        Some(app) => app,
        None => return,
    };
    let (s, v) = app.get("/", Auth::None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["name"].as_str().is_some_and(|n| !n.is_empty()));
    assert!(v["version"].as_str().is_some());
}
