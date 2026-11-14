//! Unified email delivery (final production completion, Section 2): durable ledger,
//! provider worker (retries, permanent rejection, expiry, restart recovery, idempotency),
//! signed provider webhooks, and the OTP journey (registration → ledger → provider →
//! verification → session).

mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use backend::services::email_delivery::{
    self, category, EmailWorker, NewEmail, ProviderError, RecordingProvider, WorkerSettings,
};
use base64::Engine;
use chrono::{Duration, Utc};
use common::*;
use hmac::Mac;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
}

fn settings() -> WorkerSettings {
    WorkerSettings {
        from: "CF Meet <no-reply@cfmeet.com>".into(),
        reply_to: None,
        max_attempts: 3,
        max_per_second: 50,
        batch: 10,
        retention_days: 30,
    }
}

fn email(key: &str) -> NewEmail {
    NewEmail {
        category: category::NOTIFICATION,
        idempotency_key: Some(key.into()),
        to: "ada@example.com".into(),
        to_name: Some("Ada".into()),
        subject: "Hello".into(),
        html: "<p>hi</p>".into(),
        text: "hi".into(),
        expires_at: None,
    }
}

async fn status_of(app: &TestApp, id: Uuid) -> (String, i32, bool) {
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT status, attempts, html_body IS NULL AS erased FROM email_deliveries WHERE id='{id}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    (
        row.try_get("", "status").unwrap(),
        row.try_get("", "attempts").unwrap(),
        row.try_get("", "erased").unwrap(),
    )
}

async fn make_due(app: &TestApp) {
    app.db
        .conn
        .execute_unprepared(
            "UPDATE email_deliveries SET next_attempt_at = now() - interval '1 second'",
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn ledger_is_idempotent_and_the_worker_delivers_once() {
    let app = app!();
    let db = app.state.db.clone();
    let first = email_delivery::enqueue(&db, email("k-1")).await.unwrap();
    let again = email_delivery::enqueue(&db, email("k-1")).await.unwrap();
    assert!(first.created && !again.created);
    assert_eq!(first.id, again.id);

    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(db.clone(), provider.clone(), settings());
    let r = worker.tick(Utc::now()).await.unwrap();
    assert_eq!((r.claimed, r.accepted), (1, 1));
    assert_eq!(provider.sent().len(), 1);
    assert_eq!(
        status_of(&app, first.id).await,
        ("ACCEPTED".into(), 1, true)
    );
    // Nothing left: a second worker pass (or a second instance) sends nothing.
    let r = worker.tick(Utc::now()).await.unwrap();
    assert_eq!(r.claimed, 0);
    let s = email_delivery::status(&db, first.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.provider.as_deref(), Some("recording"));
    assert!(s.accepted_at.is_some());
}

#[tokio::test]
async fn temporary_failures_retry_with_backoff_and_permanent_ones_stop() {
    let app = app!();
    let db = app.state.db.clone();
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(db.clone(), provider.clone(), settings());

    let a = email_delivery::enqueue(&db, email("temp")).await.unwrap();
    provider.fail_with(vec![ProviderError::Temporary {
        class: "NETWORK",
        status: None,
        retry_after_secs: None,
    }]);
    let r = worker.tick(Utc::now()).await.unwrap();
    assert_eq!(r.retried, 1);
    let (status, attempts, erased) = status_of(&app, a.id).await;
    assert_eq!((status.as_str(), attempts, erased), ("QUEUED", 1, false));
    // Not due yet (backoff) …
    assert_eq!(worker.tick(Utc::now()).await.unwrap().claimed, 0);
    // … then delivered.
    make_due(&app).await;
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 1);
    assert_eq!(status_of(&app, a.id).await.0, "ACCEPTED");

    let b = email_delivery::enqueue(&db, email("perm")).await.unwrap();
    provider.fail_with(vec![ProviderError::Permanent {
        class: "REJECTED",
        status: Some(422),
    }]);
    assert_eq!(worker.tick(Utc::now()).await.unwrap().failed, 1);
    assert_eq!(status_of(&app, b.id).await, ("FAILED".into(), 1, true));

    // Attempts exhausted (max 3) → FAILED, content erased.
    let c = email_delivery::enqueue(&db, email("exhaust"))
        .await
        .unwrap();
    let temp = || ProviderError::Temporary {
        class: "PROVIDER_CONFIGURATION",
        status: Some(403),
        retry_after_secs: None,
    };
    provider.fail_with(vec![temp(), temp(), temp()]);
    for _ in 0..3 {
        make_due(&app).await;
        worker.tick(Utc::now()).await.unwrap();
    }
    let (status, attempts, erased) = status_of(&app, c.id).await;
    assert_eq!((status.as_str(), attempts, erased), ("FAILED", 3, true));
    let s = email_delivery::status(&db, c.id).await.unwrap().unwrap();
    assert_eq!(
        s.last_error_class.as_deref(),
        Some("PROVIDER_CONFIGURATION")
    );
    assert_eq!(s.last_error_status, Some(403));
}

#[tokio::test]
async fn expired_messages_are_not_sent_and_abandoned_leases_are_recovered() {
    let app = app!();
    let db = app.state.db.clone();
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(db.clone(), provider.clone(), settings());

    let mut stale = email("expired");
    stale.expires_at = Some(Utc::now() - Duration::minutes(1));
    let e = email_delivery::enqueue(&db, stale).await.unwrap();
    assert_eq!(worker.tick(Utc::now()).await.unwrap().expired, 1);
    assert_eq!(status_of(&app, e.id).await.0, "EXPIRED");
    assert!(provider.sent().is_empty());

    // A worker crashed while SENDING: once its lease expires another instance resends it.
    let l = email_delivery::enqueue(&db, email("lease")).await.unwrap();
    db.execute_unprepared(&format!(
        "UPDATE email_deliveries SET status='SENDING', attempts=1, lease_holder='dead-worker',
            lease_expires_at = now() - interval '1 second' WHERE id='{}'",
        l.id
    ))
    .await
    .unwrap();
    let r = worker.tick(Utc::now()).await.unwrap();
    assert_eq!(r.accepted, 1);
    assert_eq!(status_of(&app, l.id).await, ("ACCEPTED".into(), 2, true));
    // The same ledger id is the provider idempotency key (no duplicate at the provider).
    assert_eq!(provider.sent()[0].0, l.id);
}

fn signed(secret_raw: &[u8], id: &str, body: &str) -> (String, String) {
    let ts = Utc::now().timestamp().to_string();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret_raw).unwrap();
    mac.update(format!("{id}.{ts}.{body}").as_bytes());
    let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    (ts, format!("v1,{sig}"))
}

#[tokio::test]
async fn provider_webhooks_are_authenticated_deduplicated_and_monotonic() {
    let app = app!();
    let raw = b"resend-webhook-signing-secret-32b";
    let secret = format!(
        "whsec_{}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    );
    let mut state = app.state.clone();
    state.email_webhook_secret = Some(secret);
    let router = backend::routes::create_routes(state);
    let db = app.state.db.clone();
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(db.clone(), provider.clone(), settings());
    let d = email_delivery::enqueue(&db, email("hook")).await.unwrap();
    worker.tick(Utc::now()).await.unwrap();
    let provider_id = format!("rec_{}", d.id.simple());

    let post = |id: &str, body: String, sig: Option<(String, String)>| {
        let mut req = Request::post("/api/v1/email/provider-events/resend")
            .header("content-type", "application/json")
            .header("svix-id", id);
        if let Some((ts, s)) = sig {
            req = req.header("svix-timestamp", ts).header("svix-signature", s);
        }
        req.body(Body::from(body)).unwrap()
    };
    let call = |req: Request<Body>| {
        let router = router.clone();
        async move {
            let resp = router.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    // The worker recorded the RecordingProvider id under provider "recording"; events in
    // these tests come from "resend", so attribute the delivery to it.
    db.execute_unprepared(&format!(
        "UPDATE email_deliveries SET provider='resend' WHERE id='{}'",
        d.id
    ))
    .await
    .unwrap();

    let delivered = json!({"type": "email.delivered", "created_at": "2026-11-12T10:00:00Z",
        "data": {"email_id": provider_id}})
    .to_string();
    // Unsigned / wrongly signed → 401, nothing recorded.
    let (s, _) = call(post("evt_1", delivered.clone(), None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call(post(
        "evt_1",
        delivered.clone(),
        Some(signed(
            b"another-secret-of-thirty-two-byte",
            "evt_1",
            &delivered,
        )),
    ))
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_delivery_events")
            .await,
        0
    );

    // Valid → DELIVERED.
    let (s, v) = call(post(
        "evt_1",
        delivered.clone(),
        Some(signed(raw, "evt_1", &delivered)),
    ))
    .await;
    assert_eq!((s, v["duplicate"].clone()), (StatusCode::OK, json!(false)));
    assert_eq!(status_of(&app, d.id).await.0, "DELIVERED");
    // Duplicate delivery of the same event: acknowledged, ignored.
    let (s, v) = call(post(
        "evt_1",
        delivered.clone(),
        Some(signed(raw, "evt_1", &delivered)),
    ))
    .await;
    assert_eq!((s, v["duplicate"].clone()), (StatusCode::OK, json!(true)));
    // A late "delayed" never downgrades DELIVERED; a complaint is recorded.
    let delayed =
        json!({"type": "email.delivery_delayed", "data": {"email_id": provider_id}}).to_string();
    call(post(
        "evt_2",
        delayed.clone(),
        Some(signed(raw, "evt_2", &delayed)),
    ))
    .await;
    assert_eq!(status_of(&app, d.id).await.0, "DELIVERED");
    let complained =
        json!({"type": "email.complained", "data": {"email_id": provider_id}}).to_string();
    call(post(
        "evt_3",
        complained.clone(),
        Some(signed(raw, "evt_3", &complained)),
    ))
    .await;
    assert_eq!(status_of(&app, d.id).await.0, "COMPLAINED");
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_delivery_events")
            .await,
        3
    );

    // Without a configured secret the route does not exist.
    let (s, _) = {
        let resp = app
            .router
            .clone()
            .oneshot(post(
                "evt_4",
                delivered.clone(),
                Some(signed(raw, "evt_4", &delivered)),
            ))
            .await
            .unwrap();
        (resp.status(), ())
    };
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// The OTP journey: registration records a verification email in the ledger, the
/// provider receives the code, the code verifies the address and the account signs in.
#[tokio::test]
async fn otp_journey_from_registration_to_session() {
    let app = app!();
    if !require_test_redis() {
        return;
    }
    let address = format!(
        "otp-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (s, v) = app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            json!({"email": address, "username": format!("otp{}", &Uuid::new_v4().simple().to_string()[..6]), "password": "Str0ng!Passw0rd#", "confirm_password": "Str0ng!Passw0rd#"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_deliveries WHERE category='AUTH_VERIFICATION' AND status='QUEUED'")
            .await,
        1
    );
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(app.state.db.clone(), provider.clone(), settings());
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 1);
    let (_, to, subject, text) = provider.sent()[0].clone();
    assert_eq!(to, address);
    assert_eq!(subject, "Your CF Meet verification code");
    let code: String = text
        .split_whitespace()
        .find(|w| w.len() == 6 && w.chars().all(|c| c.is_ascii_digit()))
        .expect("code in the email")
        .to_string();
    // Content (with the code) is erased once accepted.
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_deliveries WHERE category='AUTH_VERIFICATION' AND text_body IS NULL")
            .await,
        1
    );

    // Wrong code → 400; right code → verified; reused code → 400.
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let (s, _) = app
        .post(
            "/api/v1/auth/verify-email",
            Auth::None,
            json!({"email": address, "token": wrong}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = app
        .post(
            "/api/v1/auth/verify-email",
            Auth::None,
            json!({"email": address, "token": code}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = app
        .post(
            "/api/v1/auth/verify-email",
            Auth::None,
            json!({"email": address, "token": code}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Sign in → session tokens.
    let (s, v) = app
        .post(
            "/api/v1/auth/login",
            Auth::None,
            json!({"email": address, "password": "Str0ng!Passw0rd#"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["access_token"].as_str().is_some_and(|t| !t.is_empty()),
        "{v}"
    );

    // Password reset: code email in the ledger; the reset sends a security notice.
    let (s, _) = app
        .post(
            "/api/v1/auth/forgot-password",
            Auth::None,
            json!({"email": address}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    worker.tick(Utc::now()).await.unwrap();
    let (_, _, subject, text) = provider.sent().last().cloned().unwrap();
    assert_eq!(subject, "Reset your CF Meet password");
    let reset: String = text
        .split_whitespace()
        .find(|w| w.len() == 6 && w.chars().all(|c| c.is_ascii_digit()))
        .unwrap()
        .to_string();
    let (s, v) = app
        .post(
            "/api/v1/auth/reset-password",
            Auth::None,
            json!({"email": address, "token": reset, "new_password": "An0ther!Passw0rd#", "confirm_password": "An0ther!Passw0rd#"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    worker.tick(Utc::now()).await.unwrap();
    assert_eq!(
        provider.sent().last().unwrap().2,
        "Your CF Meet password was changed"
    );
}

/// The operator console notifies the applicant of a production-access decision through
/// the unified ledger (fixed template, internal token, idempotent per decision).
#[tokio::test]
async fn developer_access_decision_notice_is_queued_once() {
    let app = app!();
    std::env::set_var("INTERNAL_API_TOKEN", "test-internal-token-0123456789");
    let applicant = app.user("applicant").await;
    let request_id = Uuid::new_v4();
    let call = |body: Value, token: Option<&'static str>| {
        let router = app.router.clone();
        async move {
            let mut req = Request::post("/internal/operations/email/developer-access-decision")
                .header("content-type", "application/json");
            if let Some(t) = token {
                req = req.header("x-internal-token", t);
            }
            let resp = router
                .oneshot(req.body(Body::from(body.to_string())).unwrap())
                .await
                .unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let body = json!({"userId": applicant.id, "requestId": request_id, "decision": "APPROVED"});
    let (s, _) = call(body.clone(), None).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "hidden without the internal token"
    );
    let (s, v) = call(body.clone(), Some("test-internal-token-0123456789")).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["created"], true);
    let (_, v) = call(body, Some("test-internal-token-0123456789")).await;
    assert_eq!(v["created"], false, "one notice per decision");
    let (s, _) = call(
        json!({"userId": applicant.id, "requestId": request_id, "decision": "REJECTED", "reasonCode": "free text <b>"}),
        Some("test-internal-token-0123456789"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_deliveries WHERE category='DEVELOPER_PLATFORM'")
            .await,
        1
    );
}
