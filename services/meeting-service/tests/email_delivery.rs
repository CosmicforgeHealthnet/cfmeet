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

/// The passwordless journey: a code request records a sign-in email in the ledger, the
/// provider receives the code and link, the code signs in (creating the account), and a
/// later request's one-click link signs in too, consuming its code.
#[tokio::test]
async fn otp_journey_from_code_request_to_session() {
    let app = app!();
    if !require_test_redis() {
        return;
    }
    let address = format!(
        "otp-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let request = || {
        let app = &app;
        let address = address.clone();
        async move {
            app.post(
                "/api/v1/auth/otp/request",
                Auth::None,
                json!({ "email": address }),
            )
            .await
        }
    };
    let (s, v) = request().await;
    assert_eq!(s, StatusCode::OK, "{v}");
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
    assert_eq!(subject, "Your CF Meet sign-in code");
    let six = |text: &str| -> String {
        text.split_whitespace()
            .find(|w| w.len() == 6 && w.chars().all(|c| c.is_ascii_digit()))
            .expect("code in the email")
            .to_string()
    };
    let code = six(&text);
    assert!(text.contains("/auth/magic?email="), "{text}");
    // Content (with the code) is erased once accepted.
    assert_eq!(
        app.scalar("SELECT count(*) FROM email_deliveries WHERE category='AUTH_VERIFICATION' AND text_body IS NULL")
            .await,
        1
    );

    // Wrong code → 400; right code → signed in, new account; reused code → 400.
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let verify = |code: String| {
        let app = &app;
        let address = address.clone();
        async move {
            app.post(
                "/api/v1/auth/otp/verify",
                Auth::None,
                json!({ "email": address, "code": code }),
            )
            .await
        }
    };
    let (s, _) = verify(wrong.to_string()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = verify(code.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["is_new_account"], true);
    assert!(
        v["access_token"].as_str().is_some_and(|t| !t.is_empty()),
        "{v}"
    );
    let (s, _) = verify(code).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // The new account gets one welcome email.
    worker.tick(Utc::now()).await.unwrap();
    assert!(provider
        .sent()
        .iter()
        .any(|(_, to, subject, _)| to == &address && subject == "Welcome to CF Meet"));

    // A second request: the one-click link signs in, and its code is then used up.
    let (s, _) = request().await;
    assert_eq!(s, StatusCode::OK);
    worker.tick(Utc::now()).await.unwrap();
    let (_, _, subject, text) = provider
        .sent()
        .into_iter()
        .rev()
        .find(|(_, _, subject, _)| subject == "Your CF Meet sign-in code")
        .unwrap();
    assert_eq!(subject, "Your CF Meet sign-in code");
    let second_code = six(&text);
    let token: String = text
        .split_whitespace()
        .find_map(|w| w.split("token=").nth(1))
        .expect("magic link token")
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    assert_eq!(token.len(), 64);
    let (s, v) = app
        .post(
            "/api/v1/auth/otp/verify-link",
            Auth::None,
            json!({ "email": address, "token": token }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["is_new_account"], false);
    let (s, _) = verify(second_code).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "the link consumed its code");
    let (s, _) = app
        .post(
            "/api/v1/auth/otp/verify-link",
            Auth::None,
            json!({ "email": address, "token": token }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "the link is single use");
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

/// A provider that takes `delay` per message and records every ledger id it was asked to
/// send (duplicates included).
struct SlowProvider {
    delay: std::time::Duration,
    sent: std::sync::Mutex<Vec<Uuid>>,
}

#[async_trait::async_trait]
impl email_delivery::EmailProvider for SlowProvider {
    fn name(&self) -> &'static str {
        "slow"
    }

    async fn send(
        &self,
        m: &email_delivery::ProviderMessage<'_>,
    ) -> Result<Option<String>, ProviderError> {
        tokio::time::sleep(self.delay).await;
        self.sent.lock().unwrap().push(m.id);
        Ok(Some(format!("slow-{}", m.id)))
    }
}

/// Notification Service N0-4 (S-8): a batch that takes longer than the lease used to be
/// re-claimed by a second worker while the first was still sending it, so messages went
/// out twice. The lease is now renewed before every send and a lost row is skipped.
#[tokio::test]
async fn two_workers_never_send_a_message_twice_when_a_batch_outlives_the_lease() {
    let app = app!();
    let db = app.state.db.clone();
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(
            email_delivery::enqueue(&db, email(&format!("slow-{i}")))
                .await
                .unwrap()
                .id,
        );
    }
    make_due(&app).await;
    let provider = Arc::new(SlowProvider {
        delay: std::time::Duration::from_millis(800),
        sent: Default::default(),
    });
    // 5 messages × 0.8 s = 4 s per batch, lease 2 s: without renewal the lease lapses
    // mid-batch and the second worker re-claims the unsent rest.
    let first = EmailWorker::new(db.clone(), provider.clone(), settings()).with_lease_secs(2);
    let second = EmailWorker::new(db.clone(), provider.clone(), settings()).with_lease_secs(2);
    let a = tokio::spawn(async move { first.tick(Utc::now()).await.unwrap() });
    let mut second_reports = Vec::new();
    while !a.is_finished() {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        second_reports.push(second.tick(Utc::now()).await.unwrap());
    }
    let a = a.await.unwrap();
    second_reports.push(second.tick(Utc::now()).await.unwrap());

    let sent = provider.sent.lock().unwrap().clone();
    let mut unique = sent.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        sent.len(),
        unique.len(),
        "a message was sent twice: {sent:?}"
    );
    assert_eq!(sent.len(), 5, "every message is sent exactly once");
    assert_eq!(
        a.accepted + second_reports.iter().map(|r| r.accepted).sum::<u64>(),
        5
    );
    for id in ids {
        assert_eq!(status_of(&app, id).await.0, "ACCEPTED");
    }
}

/// Notification Service N0-12 (S-12): after a complaint, or a second bounce, reported by
/// the provider's signed webhook, new non-security mail to that address is refused with
/// SUPPRESSED; sign-in, recovery and security mail is still queued.
#[tokio::test]
async fn complained_or_twice_bounced_recipients_are_suppressed_except_for_security_mail() {
    let app = app!();
    let raw = b"resend-webhook-signing-secret-32b";
    let mut state = app.state.clone();
    state.email_webhook_secret = Some(format!(
        "whsec_{}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    ));
    let router = backend::routes::create_routes(state);
    let db = app.state.db.clone();
    let worker = EmailWorker::new(
        db.clone(),
        Arc::new(RecordingProvider::default()),
        settings(),
    );
    let to = |address: &str, key: &str, category: &'static str| NewEmail {
        category,
        idempotency_key: Some(key.into()),
        to: address.into(),
        ..email(key)
    };
    let mut n = 0;
    let mut event = |id: Uuid, kind: &str| {
        n += 1;
        let evt = format!("evt_s12_{n}");
        let body =
            json!({"type": kind, "data": {"email_id": format!("rec_{}", id.simple())}}).to_string();
        let (ts, sig) = signed(raw, &evt, &body);
        Request::post("/api/v1/email/provider-events/resend")
            .header("content-type", "application/json")
            .header("svix-id", evt)
            .header("svix-timestamp", ts)
            .header("svix-signature", sig)
            .body(Body::from(body))
            .unwrap()
    };
    let send_and_report = |id: Uuid, req: Request<Body>| {
        let (db, router, worker) = (db.clone(), router.clone(), &worker);
        async move {
            worker.tick(Utc::now()).await.unwrap();
            db.execute_unprepared(&format!(
                "UPDATE email_deliveries SET provider='resend' WHERE id='{id}'"
            ))
            .await
            .unwrap();
            assert_eq!(router.oneshot(req).await.unwrap().status(), StatusCode::OK);
        }
    };

    // Complained once.
    let c = email_delivery::enqueue(
        &db,
        to(
            "carol@example.com",
            "s12-c1",
            category::MEETING_NOTIFICATION,
        ),
    )
    .await
    .unwrap();
    let req = event(c.id, "email.complained");
    send_and_report(c.id, req).await;
    // Bounced once, then twice.
    let b1 = email_delivery::enqueue(
        &db,
        to("bob@example.com", "s12-b1", category::MEETING_INVITATION),
    )
    .await
    .unwrap();
    let req = event(b1.id, "email.bounced");
    send_and_report(b1.id, req).await;
    // One bounce is not enough.
    assert!(email_delivery::enqueue(
        &db,
        to("bob@example.com", "s12-b2", category::MEETING_NOTIFICATION)
    )
    .await
    .is_ok());
    let b2 = email_delivery::enqueue(
        &db,
        to("bob@example.com", "s12-b2", category::MEETING_NOTIFICATION),
    )
    .await
    .unwrap();
    let req = event(b2.id, "email.bounced");
    send_and_report(b2.id, req).await;

    for (address, key) in [
        ("Carol@Example.com", "s12-c2"),
        ("bob@example.com", "s12-b3"),
    ] {
        for category in [
            category::MEETING_NOTIFICATION,
            category::MEETING_INVITATION,
            category::ORGANIZATION_INVITATION,
            category::DEVELOPER_PLATFORM,
        ] {
            match email_delivery::enqueue(&db, to(address, &format!("{key}-{category}"), category))
                .await
            {
                Err(backend::error::ApiError::Conflict(m)) => {
                    assert_eq!(
                        m,
                        email_delivery::RECIPIENT_SUPPRESSED,
                        "{address} {category}"
                    )
                }
                other => panic!("{address} {category}: expected SUPPRESSED, got {other:?}"),
            }
        }
        // Account recovery, sign-in and security notices are never suppressed.
        for category in email_delivery::UNSUPPRESSIBLE_CATEGORIES {
            email_delivery::enqueue(&db, to(address, &format!("{key}-{category}"), category))
                .await
                .unwrap_or_else(|e| panic!("{address} {category} must still be queued: {e:?}"));
        }
    }
    // Other recipients are unaffected.
    assert!(email_delivery::enqueue(
        &db,
        to("dave@example.com", "s12-d", category::MEETING_NOTIFICATION)
    )
    .await
    .is_ok());
}

/// Notification Service N0-8 (S-5): Redis holds neither the code nor the address; the code
/// verifies once; a new code replaces the previous one.
#[tokio::test]
async fn otp_codes_are_stored_as_salted_hashes_under_address_digests() {
    use backend::services::otp::{email_key, OtpType};
    let app = app!();
    if !require_test_redis() {
        return;
    }
    let otp = app.state.otp_service.clone();
    let redis = app.state.redis.clone();
    let address = format!(
        "Otp-{}@Example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let hashed_key = format!("otp:signin:h:{}", email_key(&address));

    otp.store_code(&address, "482913", OtpType::SignIn)
        .await
        .unwrap();
    let stored = redis
        .get(&hashed_key)
        .await
        .unwrap()
        .expect("stored under the digest");
    assert!(!stored.contains("482913"), "no plaintext code at rest");
    assert!(
        !hashed_key.contains('@') && !hashed_key.contains("otp-"),
        "no address in the key"
    );
    assert!(otp.has_code(&address, OtpType::SignIn).await.unwrap());

    assert!(!otp
        .verify_code(&address, "000000", OtpType::SignIn)
        .await
        .unwrap());
    assert!(otp
        .verify_code(&address.to_uppercase(), "482913", OtpType::SignIn)
        .await
        .unwrap());
    assert!(
        !otp.verify_code(&address, "482913", OtpType::SignIn)
            .await
            .unwrap(),
        "single use"
    );
    assert_eq!(redis.get(&hashed_key).await.unwrap(), None);

    // A new code replaces the previous one (which then no longer verifies).
    otp.store_code(&address, "111111", OtpType::SignIn)
        .await
        .unwrap();
    otp.store_code(&address, "222222", OtpType::SignIn)
        .await
        .unwrap();
    assert!(!otp
        .verify_code(&address, "111111", OtpType::SignIn)
        .await
        .unwrap());
    assert!(otp
        .verify_code(&address, "222222", OtpType::SignIn)
        .await
        .unwrap());
    // Types never mix: a sign-in code is not a step-up code.
    otp.store_code(&address, "333333", OtpType::SignIn)
        .await
        .unwrap();
    assert!(!otp
        .verify_code(&address, "333333", OtpType::StepUp)
        .await
        .unwrap());
}

/// Insert a ledger row whose `final_at` is `final_days_ago` days in the past (`None`
/// leaves it unfinished, i.e. still QUEUED).
async fn insert_ledger_row(app: &TestApp, key: &str, final_days_ago: Option<i32>) -> Uuid {
    let id = Uuid::new_v4();
    let (status, final_at) = match final_days_ago {
        Some(d) => ("DELIVERED", format!("now() - make_interval(days => {d})")),
        None => ("QUEUED", "NULL".to_string()),
    };
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO email_deliveries (id, category, idempotency_key, recipient_hash, \
               recipient, recipient_name, subject, status, final_at) \
             VALUES ('{id}', 'NOTIFICATION', '{key}', repeat('a', 64), 'ada@example.com', \
               'Ada', 'Hello', '{status}', {final_at})"
        ))
        .await
        .unwrap();
    id
}

async fn insert_ledger_event(app: &TestApp, event_id: &str, received_days_ago: i32) {
    app.db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO email_delivery_events (provider, event_id, event_type, received_at) \
             VALUES ('resend', '{event_id}', 'email.delivered', \
               now() - make_interval(days => {received_days_ago}))"
        ))
        .await
        .unwrap();
}

/// `(exists, recipient, recipient_name)` of a ledger row.
async fn ledger_row(app: &TestApp, id: Uuid) -> (bool, Option<String>, Option<String>) {
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT recipient, recipient_name FROM email_deliveries WHERE id = '{id}'"),
        ))
        .await
        .unwrap();
    match row {
        None => (false, None, None),
        Some(r) => (
            true,
            r.try_get("", "recipient").unwrap(),
            r.try_get("", "recipient_name").unwrap(),
        ),
    }
}

async fn ledger_event_exists(app: &TestApp, event_id: &str) -> bool {
    app.db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT 1 AS one FROM email_delivery_events WHERE event_id = '{event_id}'"),
        ))
        .await
        .unwrap()
        .is_some()
}

/// A2 regression: the hourly ledger purge failed on every run in production with
/// `function make_interval(days => bigint) does not exist` (an `i64` retention bound
/// into an `integer` parameter). Runs the real purge against PostgreSQL with rows on
/// both sides of the scrub (30 days) and deletion (60 days) windows.
#[tokio::test]
async fn ledger_purge_scrubs_and_deletes_only_rows_past_the_retention_window() {
    let app = app!();
    let db = app.state.db.clone();
    let provider = Arc::new(RecordingProvider::default());

    let fresh = insert_ledger_row(&app, "purge-fresh", Some(1)).await;
    let just_inside = insert_ledger_row(&app, "purge-29d", Some(29)).await;
    let scrub_only = insert_ledger_row(&app, "purge-40d", Some(40)).await;
    let expired = insert_ledger_row(&app, "purge-70d", Some(70)).await;
    let unfinished = insert_ledger_row(&app, "purge-queued", None).await;
    insert_ledger_event(&app, "evt-fresh", 1).await;
    insert_ledger_event(&app, "evt-40d", 40).await;
    insert_ledger_event(&app, "evt-70d", 70).await;

    // (a) the purge succeeds against a real database.
    let worker = EmailWorker::new(db.clone(), provider.clone(), settings());
    worker
        .purge()
        .await
        .expect("email ledger purge must succeed on PostgreSQL");

    // (b) expired rows: addresses erased past 30 days, rows/events deleted past 60.
    assert_eq!(ledger_row(&app, scrub_only).await, (true, None, None));
    assert_eq!(ledger_row(&app, expired).await, (false, None, None));
    assert!(!ledger_event_exists(&app, "evt-70d").await);

    // (c) rows inside the window, and unfinished rows, are untouched.
    let kept = (
        true,
        Some("ada@example.com".to_string()),
        Some("Ada".to_string()),
    );
    assert_eq!(ledger_row(&app, fresh).await, kept);
    assert_eq!(ledger_row(&app, just_inside).await, kept);
    assert_eq!(ledger_row(&app, unfinished).await, kept);
    assert!(ledger_event_exists(&app, "evt-fresh").await);
    assert!(ledger_event_exists(&app, "evt-40d").await);

    // Running it again is a no-op.
    worker.purge().await.unwrap();
    assert_eq!(ledger_row(&app, fresh).await, kept);
    assert!(ledger_event_exists(&app, "evt-40d").await);

    // Out-of-range settings are clamped, never an error or a wider purge: values past
    // `i32` keep everything that is left (3650-day cap) ...
    for retention_days in [i64::MAX, i64::from(i32::MAX) + 1] {
        let w = EmailWorker::new(
            db.clone(),
            provider.clone(),
            WorkerSettings {
                retention_days,
                ..settings()
            },
        );
        w.purge().await.unwrap();
        assert_eq!(ledger_row(&app, fresh).await, kept);
        assert_eq!(ledger_row(&app, just_inside).await, kept);
        assert!(ledger_event_exists(&app, "evt-40d").await);
    }
    // ... and zero behaves like one day (never "everything up to now").
    let w = EmailWorker::new(
        db.clone(),
        provider.clone(),
        WorkerSettings {
            retention_days: 0,
            ..settings()
        },
    );
    w.purge().await.unwrap();
    assert_eq!(ledger_row(&app, just_inside).await, (false, None, None));
    assert!(
        ledger_row(&app, fresh).await.0,
        "inside the 2-day deletion window"
    );
    assert_eq!(ledger_row(&app, unfinished).await, kept);
    assert!(ledger_event_exists(&app, "evt-fresh").await);

    assert_eq!(email_delivery::retention_windows(0), (1, 2));
    assert_eq!(email_delivery::retention_windows(-5), (1, 2));
    assert_eq!(email_delivery::retention_windows(30), (30, 60));
    assert_eq!(email_delivery::retention_windows(i64::MAX), (3650, 7300));
}

/// Notification Service N5: the provider account has one webhook (this one). Events for
/// messages this ledger did not send are handed on, byte for byte with their Svix headers,
/// to the Notification Service's own verified callback; its failures make the provider
/// retry (503); a service without a callback (404) leaves the event here, as before.
#[tokio::test]
async fn events_for_messages_this_ledger_did_not_send_are_handed_to_the_notification_service() {
    use axum::{extract::State, http::HeaderMap, routing::post, Router};
    use backend::services::notification_relay::NotificationsRelay;
    use cf_service_auth::{
        KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, SystemClock,
        TokenTtl,
    };
    use std::sync::Mutex;

    let app = app!();
    let raw = b"resend-webhook-signing-secret-32b";
    let secret = format!(
        "whsec_{}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    );

    // The fake Notification Service callback: records (headers, body), answers the scripted status.
    type Seen = Arc<Mutex<(Vec<(String, String, String, String)>, Vec<u16>)>>;
    let seen: Seen = Arc::new(Mutex::new((Vec::new(), Vec::new())));
    async fn callback(State(s): State<Seen>, h: HeaderMap, body: String) -> StatusCode {
        let get = |n: &str| h.get(n).unwrap().to_str().unwrap().to_string();
        let mut inner = s.lock().unwrap();
        inner.0.push((
            get("svix-id"),
            get("svix-timestamp"),
            get("svix-signature"),
            body,
        ));
        let status = if inner.1.is_empty() {
            200
        } else {
            inner.1.remove(0)
        };
        StatusCode::from_u16(status).unwrap()
    }
    let fake = Router::new()
        .route(
            "/api/v1/notifications/provider-events/resend",
            post(callback),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, fake).await;
    });
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("meeting-service-test-1").unwrap()),
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut state = app.state.clone();
    state.email_webhook_secret = Some(secret);
    state.notifications = Some(Arc::new(NotificationsRelay::new(signer, &url).unwrap()));
    let router = backend::routes::create_routes(state);
    let db = app.state.db.clone();

    // One message of this ledger.
    let worker = EmailWorker::new(
        db.clone(),
        Arc::new(RecordingProvider::default()),
        settings(),
    );
    let own = email_delivery::enqueue(&db, email("forward-own"))
        .await
        .unwrap();
    worker.tick(Utc::now()).await.unwrap();
    db.execute_unprepared(&format!(
        "UPDATE email_deliveries SET provider='resend' WHERE id='{}'",
        own.id
    ))
    .await
    .unwrap();
    let own_message = format!("rec_{}", own.id.simple());

    let request = |evt: &str, message: &str| {
        let body = json!({"type": "email.delivered", "created_at": "2026-11-17T10:00:00Z",
            "data": {"email_id": message}})
        .to_string();
        let (ts, sig) = signed(raw, evt, &body);
        let req = Request::post("/api/v1/email/provider-events/resend")
            .header("content-type", "application/json")
            .header("svix-id", evt)
            .header("svix-timestamp", ts.clone())
            .header("svix-signature", sig.clone())
            .body(Body::from(body.clone()))
            .unwrap();
        (req, (evt.to_string(), ts, sig, body))
    };
    let status_of = |req: Request<Body>| {
        let router = router.clone();
        async move { router.oneshot(req).await.unwrap().status() }
    };

    // This ledger's message: applied here, never forwarded.
    let (req, _) = request("evt_fwd_own", &own_message);
    assert_eq!(status_of(req).await, StatusCode::OK);
    assert!(seen.lock().unwrap().0.is_empty());
    let delivered: i64 = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT count(*) AS n FROM email_deliveries WHERE id='{}' AND status='DELIVERED'",
                own.id
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap();
    assert_eq!(delivered, 1);

    // Another sender's message: handed on byte for byte.
    let (req, sent) = request("evt_fwd_other", "re_notification_service_message");
    assert_eq!(status_of(req).await, StatusCode::OK);
    assert_eq!(seen.lock().unwrap().0, vec![sent]);

    // The service fails: the provider must retry (503); then it succeeds on the retry.
    seen.lock().unwrap().1.push(503);
    let (req, _) = request("evt_fwd_retry", "re_notification_service_message_2");
    assert_eq!(status_of(req).await, StatusCode::SERVICE_UNAVAILABLE);
    let (req, _) = request("evt_fwd_retry", "re_notification_service_message_2");
    assert_eq!(status_of(req).await, StatusCode::OK);
    assert_eq!(seen.lock().unwrap().0.len(), 3);

    // No provider callback configured there (404): kept here, as before.
    seen.lock().unwrap().1.push(404);
    let (req, _) = request("evt_fwd_none", "re_unknown_message");
    assert_eq!(status_of(req).await, StatusCode::OK);
    let recorded: i64 = db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) AS n FROM email_delivery_events WHERE event_id IN ('evt_fwd_none', 'evt_fwd_other')"
                .to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap();
    assert_eq!(recorded, 2);
}

/// Gap audit M14: a calendar invite travels with the email to the provider (an
/// `invite.ics` attachment for Resend) and is erased with the bodies once sent.
#[tokio::test]
async fn calendar_invites_are_attached_and_erased_after_sending() {
    let app = app!();
    let db = app.state.db.clone();
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".to_string();
    let queued = email_delivery::enqueue_with_calendar(&db, email("k-ics"), Some(ics.clone()))
        .await
        .unwrap();
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(db.clone(), provider.clone(), settings());
    let r = worker.tick(Utc::now()).await.unwrap();
    assert_eq!(r.accepted, 1);
    assert_eq!(
        provider.calendars.lock().unwrap().clone(),
        vec![(queued.id, ics.clone())]
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM email_deliveries WHERE id = '{}' AND ics_body IS NULL",
            queued.id
        ))
        .await,
        1
    );
    let body = email_delivery::ResendProvider::body(&email_delivery::ProviderMessage {
        id: queued.id,
        category: "MEETING_INVITATION",
        from: "CF Meet <no-reply@cfmeet.com>",
        reply_to: None,
        to: "ada@example.com",
        to_name: None,
        subject: "Invitation",
        html: "<p>x</p>",
        text: "x",
        calendar: Some(&ics),
    });
    assert_eq!(body["attachments"][0]["filename"], "invite.ics");
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(body["attachments"][0]["content"].as_str().unwrap())
            .unwrap(),
        ics.as_bytes()
    );
}
