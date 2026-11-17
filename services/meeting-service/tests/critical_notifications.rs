//! Notification Service N12 — the Account CRITICAL lane (Document 4 §9.2, §9.5, §9.8,
//! §10.4): verification / reset / step-up codes and the password-changed / device-linked
//! notices routed through the fence at their legacy enqueue points, the PW1 relay to a fake
//! I1 that verifies the REAL service token and validates every envelope, the X6 / X7 owner
//! APIs called with real `cfmeet.notifications` tokens, AUTO failover, FORCED, the
//! operator's break-glass reroute and the fallback drill. Real PostgreSQL; the legacy
//! emails go through the real ledger (guard triggers armed) and the real legacy worker.
//!
//! The exactly-once property is checked from both sides in every scenario: the "service"
//! below sends only when X6 answers `GRANTED` (as `services/notifications/src/email.rs`
//! does), and the legacy worker sends whatever the ledger holds. For each message the sum
//! of both is exactly one — never two emails for one code, never zero.

mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    routing::post,
    Json, Router,
};
use backend::services::{
    critical_notifications::{
        self as critical, CriticalEvent, CriticalKeys, CriticalRelayReport, Failover, FallbackKey,
    },
    email::{device_notice_on, device_step_up_code_on, password_changed_notice_on},
    email_delivery::{self, category, EmailWorker, NewEmail, RecordingProvider, WorkerSettings},
    notification_fence::{self as fence, MeetingCategory},
    notification_owner as owner,
    notification_publications::{NotificationServiceEvents, INGEST_PATH},
};
use cf_notification_client::{validate, Envelope, SealingKey};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId,
    ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock,
    TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Duration, Utc};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
const MEETING: &str = "cfmeet.meeting-service";
const LEASES: &str = owner::LEASES_PATH;
const COMPLETIONS: &str = owner::COMPLETIONS_PATH;
const TOKEN: &str = "test-internal-token-0123456789";

macro_rules! some {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return,
        }
    };
}

const ALL: [MeetingCategory; 5] = [
    MeetingCategory::AuthVerification,
    MeetingCategory::AuthPasswordReset,
    MeetingCategory::SecurityDeviceStepUp,
    MeetingCategory::SecurityPasswordChanged,
    MeetingCategory::SecurityDeviceLinked,
];

/// The process's CRITICAL-lane keys (as `main` installs them from the environment).
fn keys() -> &'static CriticalKeys {
    let (public, _private) =
        cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
    critical::install(CriticalKeys {
        sealing: Some(SealingKey::parse(&public).unwrap()),
        fallback: Some(FallbackKey::generate()),
    });
    critical::installed().unwrap()
}

fn settings() -> WorkerSettings {
    WorkerSettings {
        from: "CF Meet <no-reply@cfmeet.com>".into(),
        reply_to: None,
        max_attempts: 3,
        max_per_second: 50,
        batch: 50,
        retention_days: 30,
    }
}

// --------------------------------------------------------------------------- fake I1

#[derive(Clone, Copy)]
enum Scripted {
    Status(u16),
    Outcome(&'static str, &'static str),
}

#[derive(Default)]
struct I1 {
    batches: Vec<Value>,
    script: Vec<Scripted>,
    /// Every following batch answers this status (an outage).
    down: Option<u16>,
    committed: BTreeSet<String>,
    invalid: Vec<String>,
}

struct I1Ctx {
    verifier: ServiceTokenVerifier,
    inner: Arc<Mutex<I1>>,
}

async fn ingest(
    State(ctx): State<Arc<I1Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let error = |status: u16, code: &str| {
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({ "error": { "code": code, "message": code } })),
        )
    };
    let Ok(caller) = ctx.verifier.verify_headers(&headers) else {
        return error(401, "INTERNAL_AUTHENTICATION_FAILED");
    };
    if !caller.has_permission(InternalPermission::NotificationsEventPublish) {
        return error(403, "INTERNAL_PERMISSION_DENIED");
    }
    let issuer = caller.authenticated_service().as_str().to_owned();
    let mut inner = ctx.inner.lock().unwrap();
    inner.batches.push(body.clone());
    if let Some(status) = inner.down {
        return error(status, "DOWN");
    }
    let scripted = (!inner.script.is_empty()).then(|| inner.script.remove(0));
    if let Some(Scripted::Status(status)) = scripted {
        return error(status, "SCRIPTED");
    }
    let mut results = Vec::new();
    for e in body["events"].as_array().cloned().unwrap_or_default() {
        let id = e["eventId"].as_str().unwrap_or_default().to_owned();
        match serde_json::from_value::<Envelope>(e.clone()) {
            Ok(env) if validate(&env, Some(&issuer)).is_ok() => {}
            other => inner.invalid.push(format!("{id}: {other:?}")),
        }
        let (outcome, reason) = match scripted {
            Some(Scripted::Outcome(o, r)) => (o, Some(r)),
            _ if inner.committed.insert(id.clone()) => ("ACCEPTED", None),
            _ => ("DUPLICATE", None),
        };
        let mut r = json!({ "eventId": id, "outcome": outcome });
        if let Some(reason) = reason {
            r["reason"] = json!(reason);
        }
        results.push(r);
    }
    (StatusCode::OK, Json(json!({ "results": results })))
}

#[derive(Clone)]
struct FakeI1 {
    url: String,
    inner: Arc<Mutex<I1>>,
}

impl FakeI1 {
    async fn start(key: &ServiceSigningKey) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::NOTIFICATIONS,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::MEETING_SERVICE,
                "audience": cf_service_auth::services::NOTIFICATIONS,
                "runtime_environment": ENV,
                "keys": [key.public_jwk()],
                "permissions": ["notifications-event.publish"],
            }],
        })
        .to_string();
        let policy = TrustPolicy::from_json(&bundle, &ServiceId::notifications(), ENV).unwrap();
        let clock: SharedClock = Arc::new(SystemClock);
        let inner = Arc::new(Mutex::new(I1::default()));
        let ctx = Arc::new(I1Ctx {
            verifier: ServiceTokenVerifier::new(policy, clock),
            inner: inner.clone(),
        });
        let app = Router::new()
            .route(INGEST_PATH, post(ingest))
            .with_state(ctx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { url, inner }
    }

    fn batches(&self) -> Vec<Value> {
        self.inner.lock().unwrap().batches.clone()
    }

    fn script(&self, s: &[Scripted]) {
        self.inner.lock().unwrap().script.extend_from_slice(s);
    }

    fn down(&self, status: Option<u16>) {
        self.inner.lock().unwrap().down = status;
    }

    fn invalid(&self) -> Vec<String> {
        self.inner.lock().unwrap().invalid.clone()
    }
}

// --------------------------------------------------------------------------- harness

fn signer(id: ServiceId, kid: &str) -> (ServiceTokenSigner, ServiceSigningKey) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (
        ServiceTokenSigner::new(id, ENV, key, TokenTtl::default(), Arc::new(SystemClock)),
        public,
    )
}

/// Meeting's receiver trust bundle: the Notification Service with `permissions`, and the
/// Developer Platform with an unrelated permission.
fn verifier(
    notifications: &ServiceSigningKey,
    permissions: &[&str],
    peer: &ServiceSigningKey,
) -> Arc<ServiceTokenVerifier> {
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({ "issuer": id, "audience": "cfmeet.meeting-service", "runtime_environment": "test",
                "keys": [k.public_jwk()], "permissions": perms })
    };
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service", "runtime_environment": "test",
    "trusted_issuers": [
        issuer("cfmeet.notifications", notifications, permissions),
        issuer("cfmeet.developer-platform", peer, &["user-session.verify"]),
    ]});
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)))
}

struct H {
    app: TestApp,
    /// Owner APIs trusting the service with every owner permission.
    owner: Router,
    /// Owner APIs trusting the service with X1 only (no `account-route-lease.claim`).
    x1_only: Router,
    service: ServiceTokenSigner,
    peer: ServiceTokenSigner,
    i1: FakeI1,
    sink: NotificationServiceEvents,
    keys: &'static CriticalKeys,
    /// Decision ids the "service" sent under a granted lease.
    service_sent: Mutex<Vec<Uuid>>,
}

impl H {
    async fn new() -> Option<Self> {
        let app = TestApp::new().await?;
        std::env::set_var("INTERNAL_API_TOKEN", TOKEN);
        let (service, s_pub) = signer(ServiceId::notifications(), "notifications-test-1");
        let (peer, p_pub) = signer(ServiceId::developer_platform(), "developer-platform-test-1");
        let owner = owner::router(
            app.state.clone(),
            verifier(
                &s_pub,
                &[
                    "account-contacts.read",
                    "meeting-notification-recipients.read",
                    "account-route-lease.claim",
                ],
                &p_pub,
            ),
        );
        let x1_only = owner::router(
            app.state.clone(),
            verifier(&s_pub, &["account-contacts.read"], &p_pub),
        );
        let meeting_key =
            ServiceSigningKey::generate(KeyId::parse("meeting-service-test-1").unwrap());
        let i1 = FakeI1::start(&meeting_key).await;
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::meeting_service(),
            ENV,
            meeting_key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ));
        let mut targets = InternalTargets::new(ENV);
        targets.insert(ServiceId::notifications(), &i1.url).unwrap();
        let sink = NotificationServiceEvents::new(Arc::new(
            InternalServiceClient::new(signer, targets, StdDuration::from_secs(5)).unwrap(),
        ));
        Some(Self {
            app,
            owner,
            x1_only,
            service,
            peer,
            i1,
            sink,
            keys: keys(),
            service_sent: Mutex::new(Vec::new()),
        })
    }

    fn db(&self) -> &sea_orm::DatabaseConnection {
        &self.app.db.conn
    }

    async fn exec(&self, sql: &str, values: Vec<sea_orm::Value>) {
        self.db()
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await
            .unwrap();
    }

    async fn set_mode(&self, category: MeetingCategory, mode: &str, canary: i16, fallback: &str) {
        self.exec(
            "UPDATE notification_routing_state
                SET mode = $2, canary_percent = $3, critical_fallback = $4,
                    routing_version = routing_version + 1, changed_by_run = 'test', changed_at = now()
              WHERE category = $1",
            vec![
                category.as_str().into(),
                mode.into(),
                canary.into(),
                fallback.into(),
            ],
        )
        .await;
    }

    async fn set_all(&self, mode: &str, fallback: &str) {
        for c in ALL {
            self.set_mode(c, mode, 100, fallback).await;
        }
    }

    async fn count(&self, sql: &str) -> i64 {
        self.app.scalar(sql).await
    }

    async fn emails_for(&self, key: &str) -> i64 {
        self.db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT count(*) AS n FROM email_deliveries WHERE idempotency_key = $1",
                [key.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap()
    }

    /// (decision id, route, reason, version, lease_count, acked, completed)
    async fn decision(&self, key: &str) -> (Uuid, String, String, i32, i32, bool, bool) {
        let r = self
            .db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT decision_id, route, reason, decision_version, lease_count,
                        ns_acked_at IS NOT NULL AS acked, ns_completed_at IS NOT NULL AS completed
                   FROM notification_route_decisions WHERE idempotency_key = $1",
                [key.into()],
            ))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no decision for {key}"));
        (
            r.try_get("", "decision_id").unwrap(),
            r.try_get("", "route").unwrap(),
            r.try_get("", "reason").unwrap(),
            r.try_get("", "decision_version").unwrap(),
            r.try_get("", "lease_count").unwrap(),
            r.try_get("", "acked").unwrap(),
            r.try_get("", "completed").unwrap(),
        )
    }

    /// (state, shadow, event, fallback length, settled)
    async fn outbox(&self, decision_id: Uuid) -> (String, bool, Value, i32, bool) {
        let r = self
            .db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT state, shadow, event, octet_length(fallback_sealed) AS len,
                        settled_at IS NOT NULL AS settled
                   FROM account_notification_outbox WHERE decision_id = $1",
                [decision_id.into()],
            ))
            .await
            .unwrap()
            .expect("outbox row");
        (
            r.try_get("", "state").unwrap(),
            r.try_get("", "shadow").unwrap(),
            r.try_get("", "event").unwrap(),
            r.try_get("", "len").unwrap(),
            r.try_get("", "settled").unwrap(),
        )
    }

    async fn fallback_blob(&self, decision_id: Uuid) -> Vec<u8> {
        self.db()
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT fallback_sealed FROM account_notification_outbox WHERE decision_id = $1",
                [decision_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "fallback_sealed")
            .unwrap()
    }

    async fn relay(&self, now: DateTime<Utc>) -> CriticalRelayReport {
        critical::relay_due(self.db(), Some(&self.sink), Some(self.keys), now)
            .await
            .unwrap()
    }

    fn signed(&self, signer: &ServiceTokenSigner, path: &str) -> Vec<(String, String)> {
        signer
            .sign_request(
                &OutboundServiceRequest::new_operation(ServiceId::meeting_service()),
                &Method::POST,
                path,
            )
            .unwrap()
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect()
    }

    async fn call(
        router: &Router,
        headers: &[(String, String)],
        path: &str,
        body: &Value,
    ) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json");
        for (k, v) in headers {
            b = b.header(k, v);
        }
        let r = router
            .clone()
            .oneshot(b.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// X6 as the Notification Service calls it.
    async fn x6(&self, ids: &[Uuid], lease_seconds: u32) -> Vec<Value> {
        let headers = self.signed(&self.service, LEASES);
        let (s, v) = Self::call(
            &self.owner,
            &headers,
            LEASES,
            &json!({ "decisionIds": ids, "leaseSeconds": lease_seconds }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["results"].as_array().unwrap().clone()
    }

    async fn x7(&self, ids: &[Uuid]) -> Vec<Value> {
        let headers = self.signed(&self.service, COMPLETIONS);
        let (s, v) = Self::call(
            &self.owner,
            &headers,
            COMPLETIONS,
            &json!({ "decisionIds": ids }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["results"].as_array().unwrap().clone()
    }

    async fn x6_one(&self, id: Uuid) -> String {
        self.x6(&[id], 30).await[0]["outcome"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn x7_one(&self, id: Uuid) -> String {
        self.x7(&[id]).await[0]["outcome"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The Notification Service's CRITICAL email worker for one decision
    /// (`services/notifications/src/email.rs`): X6, provider send only under `GRANTED`,
    /// then X7. Returns the X6 outcome.
    async fn service_attempt(&self, id: Uuid) -> String {
        let outcome = self.x6_one(id).await;
        if outcome == "GRANTED" {
            self.service_sent.lock().unwrap().push(id);
            assert_eq!(self.x7_one(id).await, "COMPLETED");
        }
        outcome
    }

    fn service_sent(&self, id: Uuid) -> usize {
        self.service_sent
            .lock()
            .unwrap()
            .iter()
            .filter(|d| **d == id)
            .count()
    }

    async fn internal(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("x-internal-token", TOKEN);
        if body.is_none() {
            b = b.header("content-length", "0");
        }
        let r = self
            .app
            .router
            .clone()
            .oneshot(
                b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// A verification code email for `user` through the real EmailService entry point.
    async fn verification(&self, user: &User, code: &str) -> String {
        self.app
            .state
            .email_service
            .send_verification_email(user.id, &user.email, "ada", code)
            .await
            .unwrap();
        verify_key(&user.email, code)
    }
}

/// The legacy key of a verification code email (`otp:verify:<recipient>:<code digest>`).
fn verify_key(email: &str, code: &str) -> String {
    use sha2::Digest;
    format!(
        "otp:verify:{}:{}",
        email_delivery::recipient_hash(email),
        hex::encode(&sha2::Sha256::digest(code.as_bytes())[..12])
    )
}

fn code_in(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_digit())
        .find(|w| w.len() == 6)
        .expect("a 6-digit code")
        .to_string()
}

fn later(seconds: i64) -> DateTime<Utc> {
    Utc::now() + Duration::seconds(seconds)
}

// ----------------------------------------------------------------------------- tests

/// LEGACY_ONLY is today's code path: the ledger row only, no decision, no outbox row,
/// the same rendered email.
#[tokio::test]
async fn legacy_only_records_exactly_the_legacy_row() {
    let h = some!(H::new().await);
    let user = h.app.user("legacy").await;
    let key = h.verification(&user, "482913").await;
    assert_eq!(h.emails_for(&key).await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM account_notification_outbox")
            .await,
        0
    );
    // The row equals what the pre-N12 enqueue wrote (same template, key, expiry).
    let r = h
        .db()
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT category, recipient, recipient_name, subject, text_body, status
               FROM email_deliveries WHERE idempotency_key = $1",
            [key.clone().into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let legacy = backend::templates::email::verification_email("ada", "482913", "24 hours");
    assert_eq!(
        r.try_get::<String>("", "category").unwrap(),
        "AUTH_VERIFICATION"
    );
    assert_eq!(r.try_get::<String>("", "recipient").unwrap(), user.email);
    assert_eq!(r.try_get::<String>("", "recipient_name").unwrap(), "ada");
    assert_eq!(
        r.try_get::<String>("", "subject").unwrap(),
        "Your CF Meet verification code"
    );
    assert_eq!(r.try_get::<String>("", "text_body").unwrap(), legacy.text);
    assert_eq!(r.try_get::<String>("", "status").unwrap(), "QUEUED");
    // A repeat of the same code collapses as today.
    h.verification(&user, "482913").await;
    assert_eq!(h.emails_for(&key).await, 1);
    assert!(h.i1.batches().is_empty());
}

/// FENCED_NEW: one transactional outbox row, no legacy row; the relay publishes it, the
/// service sends it under an X6 lease and completes it with X7; the fallback never sends.
#[tokio::test]
async fn new_decision_is_sent_once_by_the_service_under_its_lease() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("fenced").await;
    let key = h.verification(&user, "715302").await;
    let (id, route, reason, version, leases, acked, _) = h.decision(&key).await;
    assert_eq!(
        (route.as_str(), reason.as_str(), version, leases, acked),
        ("NEW", "ROUTED", 1, 0, false)
    );
    assert_eq!(h.emails_for(&key).await, 0, "never both rows (F2)");
    let (state, shadow, event, fallback_len, _) = h.outbox(id).await;
    assert_eq!((state.as_str(), shadow), ("PENDING", false));
    assert!(fallback_len > 28);
    // The envelope: catalogue-valid, ids only, the code sealed to the service key.
    let env: Envelope = serde_json::from_value(event.clone()).unwrap();
    validate(&env, Some(MEETING)).unwrap();
    assert_eq!(event["eventType"], "account.verification-code.requested");
    assert_eq!(event["route"]["decisionId"], json!(id));
    assert_eq!(
        event["recipients"][0]["ref"],
        format!("cfmeet-user:{}", user.id)
    );
    assert_eq!(event["delivery"]["idempotencyKey"], key);
    assert_eq!(event["data"]["ttlMinutes"], 1440);
    assert!(event["sealed"]["ct"].is_string());
    assert!(!event.to_string().contains("715302") && !event.to_string().contains(&user.email));
    // The fallback holds the exact legacy email (Account key only, bound to the decision).
    let fallback = h.keys.fallback.as_ref().unwrap();
    let blob = h.fallback_blob(id).await;
    assert!(!String::from_utf8_lossy(&blob).contains("715302"));
    let legacy = fallback.open(id, &blob).unwrap();
    assert_eq!(legacy.idempotency_key, key);
    assert_eq!(legacy.to, user.email);
    assert_eq!(
        legacy.text,
        backend::templates::email::verification_email("ada", "715302", "24 hours").text
    );
    assert!(fallback.open(Uuid::new_v4(), &blob).is_err());

    // PW1 → I1 ACCEPTED: acknowledged (responsibility moves to the service, §9.5).
    let report = h.relay(Utc::now()).await;
    assert_eq!(report.acked, 1);
    assert!(h.i1.invalid().is_empty(), "{:?}", h.i1.invalid());
    assert!(h.i1.batches()[0].get("shadow").is_none());
    assert!(h.decision(&key).await.5);
    assert_eq!(h.outbox(id).await.0, "ACKED");

    // X6: one lease at a time; a second claim while it is live never gets one.
    let granted = h.x6(&[id], 30).await;
    assert_eq!(granted[0]["outcome"], "GRANTED");
    let until: DateTime<Utc> = serde_json::from_value(granted[0]["leaseUntil"].clone()).unwrap();
    assert!(until > Utc::now() && until <= Utc::now() + Duration::seconds(31));
    assert_eq!(h.x6_one(id).await, "UNKNOWN");
    // A live lease blocks every failover, even far past the AUTO budget.
    assert_eq!(h.relay(later(3600)).await.failed_over, 0);
    // The provider accepted: X7 completes (terminal), idempotently.
    h.service_sent.lock().unwrap().push(id);
    assert_eq!(h.x7_one(id).await, "COMPLETED");
    assert_eq!(h.x7_one(id).await, "ALREADY_COMPLETED");
    assert_eq!(
        h.x6_one(id).await,
        "REVOKED",
        "a completed decision is never leased again"
    );
    let (state, _, event, fallback_len, settled) = h.outbox(id).await;
    assert_eq!((state.as_str(), fallback_len, settled), ("ACKED", 0, true));
    assert!(
        event.get("sealed").is_none(),
        "secrets erased once completed"
    );
    assert!(h.decision(&key).await.6);

    // Nothing ever reaches the legacy path; exactly one email in total.
    assert_eq!(h.relay(later(7200)).await, CriticalRelayReport::default());
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    worker.tick(Utc::now()).await.unwrap();
    assert!(provider.sent().is_empty());
    assert_eq!(h.emails_for(&key).await, 0);
    assert_eq!(h.service_sent(id), 1);
    // The guard trigger refuses a legacy row for a NEW decision (F2 in the database).
    let err = email_delivery::enqueue(
        h.db(),
        NewEmail {
            category: category::AUTH_VERIFICATION,
            idempotency_key: Some(key.clone()),
            to: user.email.clone(),
            to_name: None,
            subject: "x".into(),
            html: "x".into(),
            text: "x".into(),
            expires_at: None,
        },
    )
    .await;
    assert!(err.is_err());
    assert_eq!(h.emails_for(&key).await, 0);
}

/// The service never acknowledges (both replicas down): AUTO fails over after 60 s, the
/// legacy worker sends the SAME code exactly once, and the service can never send it
/// afterwards. The whole public journey: register → code from the fallback → verified.
#[tokio::test]
async fn service_never_acks_auto_failover_sends_the_same_code_once() {
    let h = some!(H::new().await);
    if !require_test_redis() {
        return;
    }
    h.set_all("FENCED_NEW", "AUTO").await;
    h.i1.down(Some(503));
    let address = format!(
        "n12-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (s, v) = h
        .app
        .post(
            "/api/v1/auth/register",
            Auth::None,
            json!({"email": address, "username": format!("n12{}", &Uuid::new_v4().simple().to_string()[..6]),
                   "password": "Str0ng!Passw0rd#", "confirm_password": "Str0ng!Passw0rd#"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(h.count("SELECT count(*) FROM email_deliveries").await, 0);
    let key: String = h
        .db()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT idempotency_key FROM notification_route_decisions WHERE category = 'AUTH_VERIFICATION'".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "idempotency_key")
        .unwrap();
    let (id, ..) = h.decision(&key).await;

    // Inside the budget: retries, no failover.
    let r = h.relay(Utc::now()).await;
    assert_eq!((r.retried, r.failed_over), (1, 0));
    assert_eq!(h.relay(later(30)).await.failed_over, 0);
    assert_eq!(h.emails_for(&key).await, 0);
    // Past 60 s without ACCEPTED or a lease: AUTO_FAILOVER, in one transaction.
    let r = h.relay(later(61)).await;
    assert_eq!(r.failed_over, 1);
    let (_, route, reason, version, leases, ..) = h.decision(&key).await;
    assert_eq!(
        (route.as_str(), reason.as_str(), version, leases),
        ("OLD", "AUTO_FAILOVER", 2, 0)
    );
    assert_eq!(h.emails_for(&key).await, 1);
    let (state, _, event, fallback_len, settled) = h.outbox(id).await;
    assert_eq!(
        (state.as_str(), fallback_len, settled),
        ("SUPERSEDED", 0, true)
    );
    assert!(event.get("sealed").is_none());
    // The service comes back: it may never send this code now.
    h.i1.down(None);
    assert_eq!(h.service_attempt(id).await, "REVOKED");
    assert_eq!(h.x7_one(id).await, "NO_LIVE_LEASE");
    assert_eq!(h.relay(later(120)).await, CriticalRelayReport::default());

    // The legacy worker sends it once; the code is the one Account stored.
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 1);
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 0);
    let sent = provider.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, address);
    assert_eq!(sent[0].2, "Your CF Meet verification code");
    let code = code_in(&sent[0].3);
    let (s, v) = h
        .app
        .post(
            "/api/v1/auth/verify-email",
            Auth::None,
            json!({"email": address, "token": code}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(h.service_sent(id), 0);
}

/// A live service lease blocks every fallback; a lease that lapsed without X7 (the service
/// crashed mid-send) is never failed over automatically — only the operator's break-glass
/// reroute under FORCED sends it again, recorded as a possible duplicate.
#[tokio::test]
async fn leased_decisions_fall_back_only_by_break_glass() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("lease").await;

    // (a) Live lease: no automatic or operator reroute.
    let live = h.verification(&user, "100001").await;
    let (live_id, ..) = h.decision(&live).await;
    h.relay(Utc::now()).await;
    assert_eq!(h.x6_one(live_id).await, "GRANTED");
    assert_eq!(h.relay(later(61)).await.failed_over, 0);
    assert_eq!(
        critical::operator_reroute(h.db(), Some(h.keys), live_id, Utc::now())
            .await
            .unwrap(),
        Failover::WaitForLease
    );
    assert_eq!(h.emails_for(&live).await, 0);

    // (b) Lapsed lease without completion.
    let lapsed = h.verification(&user, "100002").await;
    let (lapsed_id, ..) = h.decision(&lapsed).await;
    h.relay(Utc::now()).await;
    let granted = {
        let txn = h.db().begin().await.unwrap();
        let g = fence::grant_critical_lease(&txn, &lapsed, StdDuration::from_millis(50))
            .await
            .unwrap();
        txn.commit().await.unwrap();
        g
    };
    assert!(matches!(granted, fence::LeaseGrant::Granted { .. }));
    tokio::time::sleep(StdDuration::from_millis(150)).await;
    assert_eq!(
        h.relay(later(600)).await.failed_over,
        0,
        "AUTO never risks a duplicate"
    );
    assert_eq!(
        critical::operator_reroute(h.db(), Some(h.keys), lapsed_id, Utc::now())
            .await
            .unwrap(),
        Failover::Refused,
        "outside FORCED the operator cannot either"
    );
    // The lane status lists it for the operator.
    let (s, status) = h
        .internal("GET", "/internal/operations/notifications/critical", None)
        .await;
    assert_eq!(s, StatusCode::OK, "{status}");
    assert_eq!(status["leasedUncompleted"], 1);
    assert_eq!(status["leasedUncompletedDecisions"], json!([lapsed_id]));
    assert_eq!(status["leasedLive"], 1);
    assert_eq!(status["fallbackKey"], true);

    // FORCED: still nothing automatic for the leased one...
    h.set_mode(
        MeetingCategory::AuthVerification,
        "FENCED_NEW",
        100,
        "FORCED",
    )
    .await;
    assert_eq!(h.relay(later(600)).await.failed_over, 0);
    // ...until the operator's break-glass reroute (internal operation).
    let (s, v) = h
        .internal(
            "POST",
            "/internal/operations/notifications/critical/reroute",
            Some(json!({ "decisionIds": [lapsed_id, live_id] })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["results"][0]["outcome"], "REROUTED");
    assert_eq!(v["results"][0]["reason"], "REROUTED_POSSIBLE_DUPLICATE");
    assert_eq!(v["results"][1]["outcome"], "WAIT_FOR_LEASE");
    let (_, route, reason, ..) = h.decision(&lapsed).await;
    assert_eq!(
        (route.as_str(), reason.as_str()),
        ("OLD", "REROUTED_POSSIBLE_DUPLICATE")
    );
    assert_eq!(h.emails_for(&lapsed).await, 1);
    // The crashed service can neither lease nor complete it any more.
    assert_eq!(h.x6_one(lapsed_id).await, "REVOKED");
    assert_eq!(h.x7_one(lapsed_id).await, "NO_LIVE_LEASE");
    // The live one is completed by the service: still exactly one email.
    h.service_sent.lock().unwrap().push(live_id);
    assert_eq!(h.x7_one(live_id).await, "COMPLETED");
    let (s, v) = h
        .internal(
            "POST",
            "/internal/operations/notifications/critical/reroute",
            Some(json!({ "decisionIds": [live_id] })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v["results"][0]["outcome"], "REFUSED",
        "completed is terminal"
    );
    assert_eq!(h.emails_for(&live).await, 0);
    assert_eq!(h.service_sent(live_id), 1);
    // Unknown and hidden without the token.
    let (_, v) = h
        .internal(
            "POST",
            "/internal/operations/notifications/critical/reroute",
            Some(json!({ "decisionIds": [Uuid::new_v4()] })),
        )
        .await;
    assert_eq!(v["results"][0]["outcome"], "UNKNOWN");
    let (s, _) = h
        .app
        .get("/internal/operations/notifications/critical", Auth::None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// FORCED (break-glass, no restart): every new CRITICAL message goes to the legacy path at
/// once, and in-flight never-leased ones fail over without waiting for the budget.
#[tokio::test]
async fn forced_sends_every_critical_message_through_the_legacy_path() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    h.i1.down(Some(503));
    let user = h.app.user("forced").await;
    let in_flight = h.verification(&user, "200001").await;
    assert_eq!(h.decision(&in_flight).await.1, "NEW");
    h.set_all("FENCED_NEW", "FORCED").await;
    let fresh = h.verification(&user, "200002").await;
    let (_, route, reason, ..) = h.decision(&fresh).await;
    assert_eq!((route.as_str(), reason.as_str()), ("OLD", "ROUTED"));
    assert_eq!(h.emails_for(&fresh).await, 1);
    // No 60 s wait in FORCED.
    let r = h.relay(Utc::now()).await;
    assert_eq!(r.failed_over, 1);
    assert_eq!(h.decision(&in_flight).await.2, "AUTO_FAILOVER");
    assert_eq!(h.emails_for(&in_flight).await, 1);
    // OFF: never automatic, whatever the age.
    h.set_all("FENCED_NEW", "OFF").await;
    let off = h.verification(&user, "200003").await;
    assert_eq!(h.decision(&off).await.1, "NEW");
    assert_eq!(h.relay(later(3600)).await.failed_over, 0);
    assert_eq!(h.emails_for(&off).await, 0);
    // Exactly one legacy email per message, each with its own code.
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 2);
    let codes: BTreeSet<String> = provider.sent().iter().map(|s| code_in(&s.3)).collect();
    assert_eq!(
        codes,
        BTreeSet::from(["200001".to_string(), "200002".to_string()])
    );
}

/// SHADOW: the legacy row as before plus one copy the service records and never delivers
/// (posted with `shadow: true`); never leased, never failed over.
#[tokio::test]
async fn shadow_keeps_the_legacy_email_and_publishes_a_never_delivered_copy() {
    let h = some!(H::new().await);
    h.set_all("SHADOW", "AUTO").await;
    let user = h.app.user("shadow").await;
    let key = h.verification(&user, "300001").await;
    let (id, route, ..) = h.decision(&key).await;
    assert_eq!(route, "OLD");
    assert_eq!(h.emails_for(&key).await, 1);
    let (state, shadow, event, fallback_len, _) = h.outbox(id).await;
    assert_eq!((state.as_str(), shadow, fallback_len), ("PENDING", true, 0));
    validate(&serde_json::from_value(event).unwrap(), Some(MEETING)).unwrap();
    let r = h.relay(later(120)).await;
    assert_eq!((r.acked, r.failed_over), (1, 0));
    assert_eq!(h.i1.batches()[0]["shadow"], true);
    let (state, _, event, _, settled) = h.outbox(id).await;
    assert_eq!((state.as_str(), settled), ("ACKED", true));
    assert!(event.get("sealed").is_none());
    assert!(
        !h.decision(&key).await.5,
        "a shadow copy never moves responsibility"
    );
    assert_eq!(h.x6_one(id).await, "UNKNOWN");
    assert_eq!(h.emails_for(&key).await, 1);
    // A repeat is sticky: no second email, no second copy.
    h.verification(&user, "300001").await;
    assert_eq!(h.emails_for(&key).await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM account_notification_outbox")
            .await,
        1
    );
}

/// A refusal by the service fails the message over at once (it was never leased); with
/// `critical_fallback = OFF` it is a dead letter (alerted), never silently dropped.
#[tokio::test]
async fn refused_events_fail_over_at_once() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("refused").await;
    let key = h.verification(&user, "400001").await;
    h.i1.script(&[Scripted::Outcome("REJECTED", "ROUTE_NOT_NEW")]);
    let r = h.relay(Utc::now()).await;
    assert_eq!(r.failed_over, 1);
    assert_eq!(h.decision(&key).await.2, "AUTO_FAILOVER");
    assert_eq!(h.emails_for(&key).await, 1);
    // A contract refusal of the whole batch, the same.
    let batch = h.verification(&user, "400002").await;
    h.i1.script(&[Scripted::Status(422)]);
    assert_eq!(h.relay(Utc::now()).await.failed_over, 1);
    assert_eq!(h.emails_for(&batch).await, 1);
    // OFF: DEAD, counted, not sent anywhere.
    h.set_all("FENCED_NEW", "OFF").await;
    let dead = h.verification(&user, "400003").await;
    let (dead_id, ..) = h.decision(&dead).await;
    h.i1.script(&[Scripted::Outcome("REJECTED", "CATEGORY_OFF")]);
    let r = h.relay(Utc::now()).await;
    assert_eq!((r.dead, r.failed_over), (1, 0));
    assert_eq!(h.outbox(dead_id).await.0, "DEAD");
    assert_eq!(h.x6_one(dead_id).await, "REVOKED");
    let (_, status) = h
        .internal("GET", "/internal/operations/notifications/critical", None)
        .await;
    assert_eq!(status["dead"], 1);
    // DUPLICATE and HELD acknowledge (the service already has it).
    h.set_all("FENCED_NEW", "AUTO").await;
    let dup = h.verification(&user, "400004").await;
    h.i1.script(&[Scripted::Outcome("DUPLICATE", "DUPLICATE")]);
    assert_eq!(h.relay(Utc::now()).await.acked, 1);
    assert!(h.decision(&dup).await.5);
}

/// A code that expired is sent by neither path.
#[tokio::test]
async fn expired_codes_are_sent_by_neither_path() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("expiry").await;
    let mut txn = h.db().begin().await.unwrap();
    let link = Uuid::new_v4();
    let key = format!("device-step-up:{link}:0123456789abcdef");
    device_step_up_code_on(
        &txn,
        &user.email,
        "ada",
        user.id,
        Uuid::new_v4(),
        key.clone(),
        "500001",
        backend::templates::email::notification_email("ada", "s", "Your code is 500001."),
        Utc::now() + Duration::milliseconds(800),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let (id, ..) = h.decision(&key).await;
    tokio::time::sleep(StdDuration::from_millis(1200)).await;
    assert_eq!(h.x6_one(id).await, "EXPIRED");
    let r = h.relay(later(120)).await;
    assert_eq!((r.expired, r.failed_over, r.acked), (1, 0, 0));
    assert_eq!(h.outbox(id).await.0, "EXPIRED");
    assert_eq!(h.outbox(id).await.3, 0, "fallback erased");
    assert_eq!(h.emails_for(&key).await, 0);
    txn = h.db().begin().await.unwrap();
    assert_eq!(
        fence::reroute_critical(&txn, &key, false).await.unwrap(),
        fence::CriticalReroute::Refused
    );
    txn.rollback().await.unwrap();
}

/// The lease and the failover race on the same never-leased decision from separate
/// connections: every time exactly one wins — the service's lease (it sends) or the
/// failover (the legacy path sends). Never both, never neither.
#[tokio::test]
async fn lease_claims_and_failovers_race_with_exactly_one_winner() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("race").await;
    let (mut granted, mut failed_over) = (0, 0);
    for i in 0..24 {
        let key = h.verification(&user, &format!("6{:05}", i)).await;
        let (id, ..) = h.decision(&key).await;
        let db = h.db().clone();
        // Stagger the claim (0-5 ms) so both orders and true overlaps occur.
        let delay = StdDuration::from_micros((i as u64 % 6) * 1000);
        let claim = async move {
            tokio::time::sleep(delay).await;
            critical::claim_lease(&db, id, StdDuration::from_secs(30)).await
        };
        let sweep = critical::relay_due(h.db(), None, Some(h.keys), later(61));
        let (claim, sweep) = if i % 2 == 0 {
            tokio::join!(claim, sweep)
        } else {
            let (s, c) = tokio::join!(sweep, claim);
            (c, s)
        };
        let lease = claim.unwrap().outcome;
        let sweep = sweep.unwrap();
        let legacy = h.emails_for(&key).await;
        match lease {
            "GRANTED" => {
                granted += 1;
                assert_eq!((legacy, sweep.failed_over), (0, 0), "round {i}");
                assert_eq!(h.decision(&key).await.1, "NEW");
            }
            "REVOKED" => {
                failed_over += 1;
                assert_eq!((legacy, sweep.failed_over), (1, 1), "round {i}");
                assert_eq!(h.decision(&key).await.2, "AUTO_FAILOVER");
            }
            other => panic!("round {i}: unexpected X6 outcome {other}"),
        }
        // Whatever won, a later pass changes nothing.
        let after = critical::relay_due(h.db(), None, Some(h.keys), later(3600))
            .await
            .unwrap();
        assert_eq!(after.failed_over, 0, "round {i}");
    }
    eprintln!("race: {granted} leases won, {failed_over} failovers won");
    assert_eq!(granted + failed_over, 24);
}

/// X6 / X7 authentication, permission, validation and per-decision answers.
#[tokio::test]
async fn x6_x7_are_signed_permissioned_and_strict() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("x6").await;
    let key = h.verification(&user, "700001").await;
    let (id, ..) = h.decision(&key).await;
    let body = json!({ "decisionIds": [id], "leaseSeconds": 30 });
    for path in [LEASES, COMPLETIONS] {
        let b = if path == LEASES {
            body.clone()
        } else {
            json!({ "decisionIds": [id] })
        };
        // No token → 401; another peer → 403; the service without the permission → 403.
        let (s, v) = H::call(&h.owner, &[], path, &b).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{v}");
        assert_eq!(v["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
        let (s, v) = H::call(&h.owner, &h.signed(&h.peer, path), path, &b).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
        let (s, _) = H::call(&h.x1_only, &h.signed(&h.service, path), path, &b).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    // A token is single-use.
    let headers = h.signed(&h.service, LEASES);
    let (s, _) = H::call(
        &h.owner,
        &headers,
        LEASES,
        &json!({ "decisionIds": [Uuid::new_v4()], "leaseSeconds": 30 }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = H::call(&h.owner, &headers, LEASES, &body).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "replayed token");
    assert_eq!(
        h.decision(&key).await.4,
        0,
        "nothing granted by refused calls"
    );

    // Validation (400 VALIDATION_ERROR).
    let many: Vec<Uuid> = (0..101).map(|_| Uuid::new_v4()).collect();
    let bad = [
        (LEASES, json!({ "decisionIds": [], "leaseSeconds": 30 })),
        (LEASES, json!({ "decisionIds": many, "leaseSeconds": 30 })),
        (
            LEASES,
            json!({ "decisionIds": [id, id], "leaseSeconds": 30 }),
        ),
        (
            LEASES,
            json!({ "decisionIds": [id.to_string().to_uppercase()], "leaseSeconds": 30 }),
        ),
        (LEASES, json!({ "decisionIds": [id], "leaseSeconds": 4 })),
        (LEASES, json!({ "decisionIds": [id], "leaseSeconds": 31 })),
        (LEASES, json!({ "decisionIds": [id] })),
        (
            LEASES,
            json!({ "decisionIds": [id], "leaseSeconds": 30, "extra": 1 }),
        ),
        (
            COMPLETIONS,
            json!({ "decisionIds": [id], "leaseSeconds": 30 }),
        ),
        (COMPLETIONS, json!({ "decisionIds": ["not-a-uuid"] })),
    ];
    for (path, b) in bad {
        let (s, v) = H::call(&h.owner, &h.signed(&h.service, path), path, &b).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{path} {b}: {v}");
        assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    }
    assert_eq!(h.decision(&key).await.4, 0);

    // Per-decision answers, in request order: an unknown id, a STANDARD decision and a
    // shadow copy are UNKNOWN (CRITICAL lane only); a never-leased X7 is NO_LIVE_LEASE.
    let standard_key = format!("meeting-invitation:{}", Uuid::new_v4());
    let standard = {
        let txn = h.db().begin().await.unwrap();
        let d = fence::route_message(
            &txn,
            &standard_key,
            MeetingCategory::MeetingInvitation,
            later(3600),
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        d.decision_id
    };
    let unknown = Uuid::new_v4();
    assert_eq!(h.x7_one(id).await, "NO_LIVE_LEASE");
    let r = h.x6(&[unknown, id, standard], 5).await;
    assert_eq!(
        r[0],
        json!({ "decisionId": unknown, "outcome": "UNKNOWN", "leaseUntil": null })
    );
    assert_eq!(r[1]["decisionId"], json!(id));
    assert_eq!(r[1]["outcome"], "GRANTED");
    let until: DateTime<Utc> = serde_json::from_value(r[1]["leaseUntil"].clone()).unwrap();
    assert!(
        until <= Utc::now() + Duration::seconds(6),
        "leaseSeconds honoured"
    );
    assert_eq!(r[2]["outcome"], "UNKNOWN");
    assert!(
        h.decision(&key).await.5,
        "a granted lease records the acknowledgement"
    );
    let r = h.x7(&[unknown, standard, id]).await;
    assert_eq!(
        r.iter()
            .map(|x| x["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["UNKNOWN", "UNKNOWN", "COMPLETED"]
    );
}

/// Every CRITICAL legacy enqueue point routes through the fence (NEW: outbox only, with
/// the catalogue event of its category) and stays exactly the legacy row in LEGACY_ONLY.
#[tokio::test]
async fn every_critical_enqueue_point_is_fenced() {
    let h = some!(H::new().await);
    let user = h.app.user("points").await;
    let device = Uuid::new_v4();
    let es = &h.app.state.email_service;
    let mut expected = Vec::new();
    for (mode, suffix) in [("FENCED_NEW", "0001"), ("LEGACY_ONLY", "0002")] {
        h.set_all(mode, "AUTO").await;
        // `password-changed:<user>:<unix second>`: one notice per second.
        tokio::time::sleep(StdDuration::from_millis(1100)).await;
        let link = Uuid::new_v4();
        let code_v = format!("81{suffix}");
        let code_r = format!("82{suffix}");
        es.send_verification_email(user.id, &user.email, "ada", &code_v)
            .await
            .unwrap();
        es.send_password_reset_email(user.id, &user.email, "ada", &code_r)
            .await
            .unwrap();
        let txn = h.db().begin().await.unwrap();
        let step_key = format!("device-step-up:{link}:{suffix}abcdef012345");
        device_step_up_code_on(
            &txn,
            &user.email,
            "ada",
            user.id,
            device,
            step_key.clone(),
            &format!("83{suffix}"),
            backend::templates::email::notification_email("ada", "s", "m"),
            Utc::now() + Duration::minutes(10),
        )
        .await
        .unwrap();
        password_changed_notice_on(&txn, &user.email, "ada", user.id, "changed")
            .await
            .unwrap();
        device_notice_on(
            &txn,
            &user.email,
            "ada",
            user.id,
            device,
            link,
            "Linked",
            "A device was linked",
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        let reset_key = {
            use sha2::Digest;
            format!(
                "otp:reset:{}:{}",
                email_delivery::recipient_hash(&user.email),
                hex::encode(&sha2::Sha256::digest(code_r.as_bytes())[..12])
            )
        };
        let changed_key: String = h
            .db()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                if mode == "FENCED_NEW" {
                    "SELECT idempotency_key FROM notification_route_decisions
                      WHERE category = 'SECURITY_PASSWORD_CHANGED' ORDER BY decided_at DESC LIMIT 1"
                } else {
                    "SELECT idempotency_key FROM email_deliveries
                      WHERE category = 'SECURITY_PASSWORD_CHANGED' ORDER BY created_at DESC LIMIT 1"
                }
                .to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "idempotency_key")
            .unwrap();
        let cases = [
            (
                verify_key(&user.email, &code_v),
                "account.verification-code.requested",
                "AUTH_VERIFICATION",
            ),
            (
                reset_key,
                "account.password-reset-code.requested",
                "AUTH_PASSWORD_RESET",
            ),
            (
                step_key,
                "account.device-step-up-code.requested",
                "SECURITY_DEVICE_STEP_UP",
            ),
            (
                changed_key,
                "account.security.password-changed",
                "SECURITY_PASSWORD_CHANGED",
            ),
            (
                format!("device-linked:{link}"),
                "account.security.device-linked",
                "NOTIFICATION",
            ),
        ];
        for (key, event_type, ledger_category) in cases {
            if mode == "FENCED_NEW" {
                let (id, route, ..) = h.decision(&key).await;
                assert_eq!(route, "NEW", "{key}");
                assert_eq!(h.emails_for(&key).await, 0, "{key}");
                let (_, _, event, _, _) = h.outbox(id).await;
                assert_eq!(event["eventType"], event_type);
                validate(
                    &serde_json::from_value(event.clone()).unwrap(),
                    Some(MEETING),
                )
                .unwrap();
                let legacy = h
                    .keys
                    .fallback
                    .as_ref()
                    .unwrap()
                    .open(id, &h.fallback_blob(id).await)
                    .unwrap();
                assert_eq!(legacy.category, ledger_category);
                assert_eq!(legacy.idempotency_key, key);
                expected.push(key);
            } else {
                assert_eq!(h.emails_for(&key).await, 1, "{key}");
                let decided = h
                    .db()
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT count(*) AS n FROM notification_route_decisions WHERE idempotency_key = $1",
                        [key.clone().into()],
                    ))
                    .await
                    .unwrap()
                    .unwrap()
                    .try_get::<i64>("", "n")
                    .unwrap();
                assert_eq!(decided, 0, "LEGACY_ONLY records no decision: {key}");
            }
        }
    }
    assert_eq!(expected.len(), 5);
    assert_eq!(
        h.count("SELECT count(*) FROM account_notification_outbox")
            .await,
        5
    );
    // All five relay and acknowledge in one batch.
    assert_eq!(h.relay(Utc::now()).await.acked, 5);
    assert!(h.i1.invalid().is_empty(), "{:?}", h.i1.invalid());
}

/// Without the Account fallback key (or the service key for a code) a CRITICAL message is
/// never decided NEW: it stays on the legacy path (`UNPUBLISHABLE`).
#[tokio::test]
async fn without_keys_critical_messages_stay_on_the_legacy_path() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("nokeys").await;
    let email = |code: &str| NewEmail {
        category: category::AUTH_VERIFICATION,
        idempotency_key: Some(verify_key(&user.email, code)),
        to: user.email.clone(),
        to_name: Some("ada".into()),
        subject: "Your CF Meet verification code".into(),
        html: "<p>x</p>".into(),
        text: "x".into(),
        expires_at: Some(later(3600)),
    };
    let no_fallback = CriticalKeys {
        sealing: h.keys.sealing.clone(),
        fallback: None,
    };
    let no_sealing = CriticalKeys {
        sealing: None,
        fallback: Some(FallbackKey::generate()),
    };
    for (code, keys) in [
        ("900001", None),
        ("900002", Some(&no_fallback)),
        ("900003", Some(&no_sealing)),
    ] {
        let e = critical::enqueue(
            h.db(),
            keys,
            email(code),
            CriticalEvent::verification_code(user.id, code, 60),
        )
        .await
        .unwrap();
        assert!(e.created);
        let key = verify_key(&user.email, code);
        let (_, route, reason, ..) = h.decision(&key).await;
        assert_eq!(
            (route.as_str(), reason.as_str()),
            ("OLD", "UNPUBLISHABLE"),
            "{code}"
        );
        assert_eq!(h.emails_for(&key).await, 1);
    }
    // A security notice needs no service key: NEW with the fallback key alone.
    let e = critical::enqueue(
        h.db(),
        Some(&no_sealing),
        NewEmail {
            category: category::SECURITY_PASSWORD_CHANGED,
            idempotency_key: Some(format!("password-changed:{}:1", user.id)),
            ..email("900004")
        },
        CriticalEvent::password_changed(user.id, Utc::now()),
    )
    .await
    .unwrap();
    assert!(e.created);
    assert_eq!(
        h.decision(&format!("password-changed:{}:1", user.id))
            .await
            .1,
        "NEW"
    );
    // An unusable address answers exactly as the legacy enqueue (and decides nothing).
    let err = critical::enqueue(
        h.db(),
        Some(h.keys),
        NewEmail {
            to: "not an address".into(),
            ..email("900005")
        },
        CriticalEvent::verification_code(user.id, "900005", 60),
    )
    .await;
    assert!(matches!(err, Err(backend::error::ApiError::BadRequest(_))));
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE category = 'AUTH_VERIFICATION'").await,
        3
    );
}

/// The fallback drill (`email-acceptance confirm=fallback`): an acceptance email through the
/// exact failover path, sent by the legacy worker; `/health` reports the key.
#[tokio::test]
async fn fallback_drill_goes_through_the_legacy_worker() {
    let h = some!(H::new().await);
    let (s, v) = h
        .internal(
            "POST",
            "/internal/operations/email/fallback-drill",
            Some(json!({ "to": "drill@example.com", "reference": "n12-drill-1" })),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["created"], true);
    assert_eq!(v["path"], "LEGACY_FALLBACK");
    let delivery: Uuid = serde_json::from_value(v["deliveryId"].clone()).unwrap();
    let (_, again) = h
        .internal(
            "POST",
            "/internal/operations/email/fallback-drill",
            Some(json!({ "to": "drill@example.com", "reference": "n12-drill-1" })),
        )
        .await;
    assert_eq!(again["created"], false, "one drill email per reference");
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 1);
    assert_eq!(provider.sent()[0].0, delivery);
    assert_eq!(
        provider.sent()[0].2,
        "CF Meet fallback email drill n12-drill-1"
    );
    let (s, status) = h
        .internal(
            "GET",
            &format!("/internal/operations/email/deliveries/{delivery}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(status["status"], "ACCEPTED");
    assert_eq!(status["category"], "OPERATIONS_ACCEPTANCE");
    let (_, health) = h.app.get("/health", Auth::None).await;
    assert_eq!(health["criticalFallbackKey"], true);
    // Hidden without the internal token.
    let (s, _) = h
        .app
        .post(
            "/internal/operations/email/fallback-drill",
            Auth::None,
            json!({ "to": "drill@example.com", "reference": "x" }),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Maintenance purges settled rows after the retention window, then their decisions.
#[tokio::test]
async fn settled_rows_are_purged_after_retention() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("purge").await;
    let key = h.verification(&user, "950001").await;
    let (id, ..) = h.decision(&key).await;
    h.relay(Utc::now()).await;
    assert_eq!(h.service_attempt(id).await, "GRANTED");
    assert_eq!(
        critical::maintain(h.db(), 100, Utc::now()).await.unwrap(),
        (0, 0)
    );
    // The settled row goes; its decision stays until it expires on the database clock
    // (the F3 trigger refuses an earlier delete).
    let (rows, decisions) = critical::maintain(h.db(), 100, later(40 * 86_400))
        .await
        .unwrap();
    assert_eq!((rows, decisions), (1, 0));
    assert_eq!(
        h.count("SELECT count(*) FROM account_notification_outbox")
            .await,
        0
    );
    assert!(h.decision(&key).await.6);
}
