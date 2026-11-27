//! Notification Service N12 — the Account CRITICAL lane (Document 4 §9.2, §9.5, §9.8,
//! §10.4): the one-time codes (passwordless sign-in, email change and step-up action codes
//! of PR #97, device step-up codes) and the device-linked notice routed through the fence
//! at their legacy enqueue points, the PW1 relay to a fake
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
    email::{device_notice_on, device_step_up_code_on},
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

const ALL: [MeetingCategory; 6] = [
    MeetingCategory::AuthVerification,
    MeetingCategory::AuthPasswordReset,
    MeetingCategory::SecurityDeviceStepUp,
    MeetingCategory::SecurityPasswordChanged,
    MeetingCategory::SecurityDeviceLinked,
    MeetingCategory::AccountNotices,
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

    /// A device step-up code email for `user` (the code the catalogue carries:
    /// `account.device-step-up-code.requested`) through its real enqueue point,
    /// `email::device_step_up_code_on`; the code determines the link, so a repeat is the
    /// same message.
    async fn code_email(&self, user: &User, code: &str) -> String {
        let link = Uuid::from_u128(user.id.as_u128() ^ code.parse::<u128>().unwrap());
        let key = step_up_key(link, code);
        device_step_up_code_on(
            self.db(),
            &user.email,
            "ada",
            user.id,
            device_of(user),
            key.clone(),
            code,
            step_up_template(code),
            Utc::now() + Duration::minutes(10),
        )
        .await
        .unwrap();
        key
    }
}

/// The account device a test user's step-up code is for.
fn device_of(user: &User) -> Uuid {
    Uuid::from_u128(!user.id.as_u128())
}

/// The legacy key of a device step-up code (`device-step-up:<link>:<code digest>`).
fn step_up_key(link: Uuid, code: &str) -> String {
    use sha2::Digest;
    format!(
        "device-step-up:{link}:{}",
        &hex::encode(sha2::Sha256::digest(code.as_bytes()))[..16]
    )
}

fn step_up_template(code: &str) -> backend::templates::email::EmailTemplate {
    backend::templates::email::notification_email(
        "ada",
        "Your CF Meet device verification code",
        &format!(
            "Your code is {code}. It confirms a request to trust a new browser for your CF Meet account. It expires in 10 minutes."
        ),
    )
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
    let key = h.code_email(&user, "482913").await;
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
    let legacy = step_up_template("482913");
    assert_eq!(
        r.try_get::<String>("", "category").unwrap(),
        "SECURITY_DEVICE_STEP_UP"
    );
    assert_eq!(r.try_get::<String>("", "recipient").unwrap(), user.email);
    assert_eq!(r.try_get::<String>("", "recipient_name").unwrap(), "ada");
    assert_eq!(
        r.try_get::<String>("", "subject").unwrap(),
        "Your CF Meet device verification code"
    );
    assert_eq!(r.try_get::<String>("", "text_body").unwrap(), legacy.text);
    assert_eq!(r.try_get::<String>("", "status").unwrap(), "QUEUED");
    // A repeat of the same code collapses as today.
    h.code_email(&user, "482913").await;
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
    let key = h.code_email(&user, "715302").await;
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
    assert_eq!(event["eventType"], "account.device-step-up-code.requested");
    assert_eq!(event["route"]["decisionId"], json!(id));
    assert_eq!(
        event["recipients"][0]["ref"],
        format!("cfmeet-user:{}", user.id)
    );
    assert_eq!(event["delivery"]["idempotencyKey"], key);
    assert!((9..=10).contains(&event["data"]["ttlMinutes"].as_i64().unwrap()));
    assert_eq!(
        event["data"]["accountDeviceId"],
        device_of(&user).to_string()
    );
    assert!(event["sealed"]["ct"].is_string());
    assert!(!event.to_string().contains("715302") && !event.to_string().contains(&user.email));
    // The fallback holds the exact legacy email (Account key only, bound to the decision).
    let fallback = h.keys.fallback.as_ref().unwrap();
    let blob = h.fallback_blob(id).await;
    assert!(!String::from_utf8_lossy(&blob).contains("715302"));
    let legacy = fallback.open(id, &blob).unwrap();
    assert_eq!(legacy.idempotency_key, key);
    assert_eq!(legacy.to, user.email);
    assert_eq!(legacy.text, step_up_template("715302").text);
    assert_eq!(legacy.category, "SECURITY_DEVICE_STEP_UP");
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
    assert_eq!(h.relay(later(300)).await.failed_over, 0);
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
            category: category::SECURITY_DEVICE_STEP_UP,
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
/// afterwards.
#[tokio::test]
async fn service_never_acks_auto_failover_sends_the_same_code_once() {
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    h.i1.down(Some(503));
    let user = h.app.user("failover").await;
    let address = user.email.clone();
    let key = h.code_email(&user, "591734").await;
    assert_eq!(h.count("SELECT count(*) FROM email_deliveries").await, 0);
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
    assert_eq!(sent[0].2, "Your CF Meet device verification code");
    assert_eq!(code_in(&sent[0].3), "591734", "the same code, once");
    assert_eq!(h.service_sent(id), 0);
}

/// Passwordless sign-in (PR #97) under FENCED_NEW / AUTO: the sign-in code is decided NEW
/// (`account.sign-in-code.requested`: one email principal, no subject, code, magic-link
/// token and address sealed, the exact legacy email in `fallback_sealed`). The service
/// never acknowledges it, so AUTO fails it over after 60 s: the legacy worker sends the
/// SAME code once (the service can never send it afterwards) and the public journey
/// (request → emailed code → verify) works unchanged.
#[tokio::test]
async fn passwordless_sign_in_codes_go_new_and_fail_over_with_the_same_code() {
    let h = some!(H::new().await);
    if !require_test_redis() {
        return;
    }
    h.set_all("FENCED_NEW", "AUTO").await;
    let address = format!(
        "n12-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (s, v) = h
        .app
        .post(
            "/api/v1/auth/otp/request",
            Auth::None,
            json!({ "email": address }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let key: String = h
        .db()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT idempotency_key FROM notification_route_decisions WHERE category = 'AUTH_VERIFICATION'"
                .to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "idempotency_key")
        .unwrap();
    assert!(key.starts_with("otp:signin:"), "{key}");
    let (id, route, reason, ..) = h.decision(&key).await;
    assert_eq!((route.as_str(), reason.as_str()), ("NEW", "ROUTED"));
    assert_eq!(h.emails_for(&key).await, 0, "never both rows");
    let (state, _, event, _, _) = h.outbox(id).await;
    assert_eq!(state, "PENDING");
    assert_eq!(event["eventType"], "account.sign-in-code.requested");
    validate(
        &serde_json::from_value(event.clone()).unwrap(),
        Some(MEETING),
    )
    .unwrap();
    assert_eq!(
        event["recipients"][0]["ref"],
        owner::email_principal_ref(&address)
    );
    assert!(event.get("subject").is_none());
    assert!(!event.to_string().contains(&address));
    let legacy = h
        .keys
        .fallback
        .as_ref()
        .unwrap()
        .open(id, &h.fallback_blob(id).await)
        .unwrap();
    assert_eq!(legacy.subject, "Your CF Meet sign-in code");
    assert!(legacy.text.contains("/auth/magic?email="));
    // The service is down: no acknowledgement, no lease; AUTO fails over after 60 s.
    h.i1.down(Some(503));
    assert_eq!(h.relay(later(30)).await.failed_over, 0);
    assert_eq!(h.relay(later(61)).await.failed_over, 1);
    let (_, route, reason, ..) = h.decision(&key).await;
    assert_eq!((route.as_str(), reason.as_str()), ("OLD", "AUTO_FAILOVER"));
    assert_eq!(h.emails_for(&key).await, 1);
    h.i1.down(None);
    assert_eq!(h.service_attempt(id).await, "REVOKED");
    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 1);
    assert_eq!(worker.tick(Utc::now()).await.unwrap().accepted, 0);
    let sent = provider.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].2, "Your CF Meet sign-in code");
    assert_eq!(sent[0].3, legacy.text, "the legacy email, byte for byte");
    let code = sent[0]
        .3
        .lines()
        .map(str::trim)
        .find(|l| l.len() == 6 && l.bytes().all(|b| b.is_ascii_digit()))
        .expect("a code line")
        .to_string();
    let (s, v) = h
        .app
        .post(
            "/api/v1/auth/otp/verify",
            Auth::None,
            json!({ "email": address, "code": code }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(
        v["access_token"].as_str().is_some_and(|t| !t.is_empty()),
        "{v}"
    );
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
    let live = h.code_email(&user, "100001").await;
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
    let lapsed = h.code_email(&user, "100002").await;
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
        h.relay(later(300)).await.failed_over,
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
        MeetingCategory::SecurityDeviceStepUp,
        "FENCED_NEW",
        100,
        "FORCED",
    )
    .await;
    assert_eq!(h.relay(later(300)).await.failed_over, 0);
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
    let in_flight = h.code_email(&user, "200001").await;
    assert_eq!(h.decision(&in_flight).await.1, "NEW");
    h.set_all("FENCED_NEW", "FORCED").await;
    let fresh = h.code_email(&user, "200002").await;
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
    let off = h.code_email(&user, "200003").await;
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
    let key = h.code_email(&user, "300001").await;
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
    h.code_email(&user, "300001").await;
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
    let key = h.code_email(&user, "400001").await;
    h.i1.script(&[Scripted::Outcome("REJECTED", "ROUTE_NOT_NEW")]);
    let r = h.relay(Utc::now()).await;
    assert_eq!(r.failed_over, 1);
    assert_eq!(h.decision(&key).await.2, "AUTO_FAILOVER");
    assert_eq!(h.emails_for(&key).await, 1);
    // A contract refusal of the whole batch, the same.
    let batch = h.code_email(&user, "400002").await;
    h.i1.script(&[Scripted::Status(422)]);
    assert_eq!(h.relay(Utc::now()).await.failed_over, 1);
    assert_eq!(h.emails_for(&batch).await, 1);
    // OFF: DEAD, counted, not sent anywhere.
    h.set_all("FENCED_NEW", "OFF").await;
    let dead = h.code_email(&user, "400003").await;
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
    let dup = h.code_email(&user, "400004").await;
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
        let key = h.code_email(&user, &format!("6{:05}", i)).await;
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
    let key = h.code_email(&user, "700001").await;
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

/// Every CRITICAL enqueue point that exists after PR #97 routes through the fence, and
/// since contract 1.1.0 every one has a catalogue event: in FENCED_NEW each travels NEW
/// (outbox only; the code — and the magic-link token and the address where the catalogue
/// seals them — sealed to the service key; the exact legacy email in `fallback_sealed`).
/// In LEGACY_ONLY every one is exactly its PR #97 legacy row, byte for byte, with no
/// decision.
#[tokio::test]
async fn every_critical_enqueue_point_is_fenced() {
    use backend::{
        services::step_up::StepUpPurpose,
        templates::email::{email_change_code_email, sign_in_code_email, step_up_code_email},
    };
    let h = some!(H::new().await);
    let user = h.app.user("points").await;
    let device = Uuid::new_v4();
    let es = &h.app.state.email_service;
    let details = backend::templates::email::RequestDetails {
        client: Some("TestBrowser/1.0".into()),
        address: Some("102.89.x.x".into()),
        when_utc: "2026-11-17 12:00".into(),
    };
    let digest = |code: &str| {
        use sha2::Digest;
        hex::encode(&sha2::Sha256::digest(code.as_bytes())[..12])
    };
    let token = "0123456789abcdef0123456789abcdef";
    let mut published = 0;
    for (mode, suffix) in [("FENCED_NEW", "0001"), ("LEGACY_ONLY", "0002")] {
        h.set_all(mode, "AUTO").await;
        let link = Uuid::new_v4();
        let new_address = format!(
            "new-{}@example.com",
            &Uuid::new_v4().simple().to_string()[..8]
        );
        let (sign_in, change, action, step) = (
            format!("81{suffix}"),
            format!("82{suffix}"),
            format!("83{suffix}"),
            format!("84{suffix}"),
        );
        es.send_sign_in_code(&user.email, &sign_in, Some(token), &details)
            .await
            .unwrap();
        es.send_email_change_code(&new_address, "ada", user.id, &change)
            .await
            .unwrap();
        es.send_step_up_code(
            &user.email,
            "ada",
            user.id,
            &action,
            StepUpPurpose::ChangeEmail,
        )
        .await
        .unwrap();
        let txn = h.db().begin().await.unwrap();
        let step_key = step_up_key(link, &step);
        device_step_up_code_on(
            &txn,
            &user.email,
            "ada",
            user.id,
            device,
            step_key.clone(),
            &step,
            step_up_template(&step),
            Utc::now() + Duration::minutes(10),
        )
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
        let hash = |a: &str| email_delivery::recipient_hash(a);
        let magic = format!(
            "{}/auth/magic?email={}&token={token}",
            es.app_url().trim_end_matches('/'),
            user.email.replace('@', "%40")
        );
        // The PR #97 legacy emails of the three account codes (subject, recipient, name,
        // rendering), which LEGACY_ONLY records and a failover restores byte for byte.
        let legacy: [(
            String,
            &str,
            String,
            Option<&str>,
            backend::templates::email::EmailTemplate,
        ); 3] = [
            (
                format!("otp:signin:{}:{}", hash(&user.email), digest(&sign_in)),
                "Your CF Meet sign-in code",
                user.email.clone(),
                None,
                sign_in_code_email(&sign_in, "10 minutes", Some(&magic), &details),
            ),
            (
                format!("otp:emailchange:{}:{}", hash(&new_address), digest(&change)),
                "Confirm your new CF Meet email address",
                new_address.clone(),
                Some("ada"),
                email_change_code_email("ada", &change, "10 minutes"),
            ),
            (
                format!("otp:stepup:{}:{}", hash(&user.email), digest(&action)),
                "Confirm it's you",
                user.email.clone(),
                Some("ada"),
                step_up_code_email(
                    "ada",
                    &action,
                    "10 minutes",
                    "change the email address of your CF Meet account",
                ),
            ),
        ];
        // (key, catalogue event, legacy ledger category)
        let cases = [
            (
                legacy[0].0.clone(),
                "account.sign-in-code.requested",
                "AUTH_VERIFICATION",
            ),
            (
                legacy[1].0.clone(),
                "account.email-change-code.requested",
                "AUTH_VERIFICATION",
            ),
            (
                legacy[2].0.clone(),
                "account.step-up-code.requested",
                "SECURITY_DEVICE_STEP_UP",
            ),
            (
                step_key,
                "account.device-step-up-code.requested",
                "SECURITY_DEVICE_STEP_UP",
            ),
            (
                format!("device-linked:{link}"),
                "account.security.device-linked",
                "NOTIFICATION",
            ),
        ];
        for (key, event_type, ledger_category) in cases {
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
            let expected = legacy.iter().find(|l| l.0 == key);
            if mode == "LEGACY_ONLY" {
                assert_eq!(h.emails_for(&key).await, 1, "{key}");
                assert_eq!(decided, 0, "LEGACY_ONLY records no decision: {key}");
                if let Some((_, subject, to, name, t)) = expected {
                    let r = h
                        .db()
                        .query_one(Statement::from_sql_and_values(
                            DbBackend::Postgres,
                            "SELECT category, recipient, recipient_name, subject, text_body, html_body
                               FROM email_deliveries WHERE idempotency_key = $1",
                            [key.clone().into()],
                        ))
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        r.try_get::<String>("", "category").unwrap(),
                        ledger_category
                    );
                    assert_eq!(r.try_get::<String>("", "recipient").unwrap(), *to);
                    assert_eq!(
                        r.try_get::<Option<String>>("", "recipient_name")
                            .unwrap()
                            .as_deref(),
                        *name
                    );
                    assert_eq!(r.try_get::<String>("", "subject").unwrap(), *subject);
                    assert_eq!(
                        r.try_get::<String>("", "text_body").unwrap(),
                        t.text,
                        "{key}"
                    );
                    assert_eq!(
                        r.try_get::<String>("", "html_body").unwrap(),
                        t.html,
                        "{key}"
                    );
                }
                continue;
            }
            assert_eq!(decided, 1, "{key}");
            let (id, route, reason, ..) = h.decision(&key).await;
            assert_eq!(
                (route.as_str(), reason.as_str()),
                ("NEW", "ROUTED"),
                "{key}"
            );
            assert_eq!(h.emails_for(&key).await, 0, "{key}");
            let (_, _, event, _, _) = h.outbox(id).await;
            assert_eq!(event["eventType"], event_type);
            validate(
                &serde_json::from_value(event.clone()).unwrap(),
                Some(MEETING),
            )
            .unwrap();
            let opened = h
                .keys
                .fallback
                .as_ref()
                .unwrap()
                .open(id, &h.fallback_blob(id).await)
                .unwrap();
            assert_eq!(opened.category, ledger_category);
            assert_eq!(opened.idempotency_key, key);
            if let Some((_, subject, to, name, t)) = expected {
                // The fallback is exactly the LEGACY_ONLY email.
                assert_eq!(
                    (
                        opened.subject.as_str(),
                        &opened.to,
                        opened.to_name.as_deref()
                    ),
                    (*subject, to, *name)
                );
                assert_eq!((&opened.text, &opened.html), (&t.text, &t.html), "{key}");
                // Nothing secret in the envelope in clear: code, token, address.
                let clear = event.to_string();
                for secret in [&sign_in, &change, &action, &user.email, &new_address] {
                    assert!(!clear.contains(secret.as_str()), "{key}");
                }
                assert!(!clear.contains(token));
                assert!(event["sealed"]["ct"].is_string());
                assert_eq!(event["data"]["ttlMinutes"], 10);
            }
            match event_type {
                "account.sign-in-code.requested" => {
                    assert_eq!(
                        event["recipients"][0]["ref"],
                        owner::email_principal_ref(&user.email)
                    );
                    assert!(event.get("subject").is_none(), "no account is named");
                    assert_eq!(event["data"]["requestedAt"], "2026-11-17T12:00:00Z");
                    assert_eq!(event["data"]["requestClient"], "TestBrowser/1.0");
                    assert_eq!(event["data"]["requestNetwork"], "102.89.x.x");
                }
                "account.email-change-code.requested" => {
                    assert_eq!(
                        event["recipients"][0]["ref"],
                        owner::email_principal_ref(&new_address)
                    );
                    assert_eq!(event["subject"]["id"], json!(user.id));
                }
                "account.step-up-code.requested" => {
                    assert_eq!(
                        event["recipients"][0]["ref"],
                        format!("cfmeet-user:{}", user.id)
                    );
                    assert_eq!(event["data"]["purpose"], "CHANGE_EMAIL");
                }
                _ => {}
            }
            published += 1;
        }
    }
    assert_eq!(published, 5);
    assert_eq!(
        h.count("SELECT count(*) FROM account_notification_outbox")
            .await,
        5
    );
    assert_eq!(h.relay(Utc::now()).await.acked, 5);
    assert!(h.i1.invalid().is_empty(), "{:?}", h.i1.invalid());
}

/// The email-change and step-up action codes in FENCED_NEW / AUTO, like the device
/// step-up code: one is sent by the service under its X6 lease (X7 completes it; the
/// legacy path never sends it), the other is never acknowledged and AUTO fails it over
/// after 60 s, so the legacy worker sends the SAME code once and the service never can.
#[tokio::test]
async fn email_change_and_step_up_codes_go_new_and_fail_over_with_the_same_code() {
    use backend::services::step_up::StepUpPurpose;
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("pw-codes").await;
    let es = &h.app.state.email_service;
    let digest = |code: &str| {
        use sha2::Digest;
        hex::encode(&sha2::Sha256::digest(code.as_bytes())[..12])
    };
    let new_address = format!(
        "moved-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    es.send_email_change_code(&new_address, "ada", user.id, "604213")
        .await
        .unwrap();
    let change_key = format!(
        "otp:emailchange:{}:{}",
        email_delivery::recipient_hash(&new_address),
        digest("604213")
    );
    let (change_id, route, ..) = h.decision(&change_key).await;
    assert_eq!(route, "NEW");
    // Delivered by the service: I1 accepts, X6 grants, X7 completes.
    assert_eq!(h.relay(Utc::now()).await.acked, 1);
    assert_eq!(h.service_attempt(change_id).await, "GRANTED");
    assert!(h.decision(&change_key).await.6, "completed");

    // The step-up code: the service never acknowledges it.
    h.i1.down(Some(503));
    es.send_step_up_code(
        &user.email,
        "ada",
        user.id,
        "731904",
        StepUpPurpose::DeactivateAccount,
    )
    .await
    .unwrap();
    let step_key = format!(
        "otp:stepup:{}:{}",
        email_delivery::recipient_hash(&user.email),
        digest("731904")
    );
    let (step_id, route, ..) = h.decision(&step_key).await;
    assert_eq!(route, "NEW");
    assert_eq!(h.relay(later(30)).await.failed_over, 0);
    assert_eq!(h.relay(later(61)).await.failed_over, 1);
    let (_, route, reason, ..) = h.decision(&step_key).await;
    assert_eq!((route.as_str(), reason.as_str()), ("OLD", "AUTO_FAILOVER"));
    h.i1.down(None);
    assert_eq!(h.service_attempt(step_id).await, "REVOKED");
    assert_eq!(h.relay(later(7200)).await.failed_over, 0);

    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    worker.tick(Utc::now()).await.unwrap();
    worker.tick(Utc::now()).await.unwrap();
    let sent = provider.sent();
    assert_eq!(
        sent.len(),
        1,
        "only the failed-over code reaches the legacy path"
    );
    assert_eq!(sent[0].1, user.email);
    assert_eq!(sent[0].2, "Confirm it's you");
    assert_eq!(code_in(&sent[0].3), "731904", "the same code, once");
    assert!(sent[0].3.contains("deactivate your CF Meet account"));
    assert_eq!((h.service_sent(change_id), h.service_sent(step_id)), (1, 0));
    assert_eq!(
        (
            h.emails_for(&change_key).await,
            h.emails_for(&step_key).await
        ),
        (0, 1)
    );
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
    let key = h.code_email(&user, "950001").await;
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

// ------------------------------------------------------------------- account notices

/// The legacy email one account notice enqueue point records (what `LEGACY_ONLY` writes and
/// what a NEW decision's `fallback_sealed` must hold, byte for byte).
struct LegacyNotice {
    key: String,
    category: &'static str,
    to: String,
    name: Option<String>,
    subject: &'static str,
    /// Renderings accepted for the body (the email-changed notice shows the minute of the
    /// change: the minute before and after the call).
    renderings: Vec<backend::templates::email::EmailTemplate>,
    event_type: &'static str,
}

/// The five account notice enqueue points (+ the support-assisted email change), through
/// their real code: `send_welcome_email`, `send_new_sign_in_alert`,
/// `send_email_in_use_notice` and `email_changed_notices_on` (self-service with the undo
/// token, support-assisted without), the last on the caller's transaction as
/// `email_change` does. Returns the legacy emails they must record.
async fn account_notices(
    h: &H,
    user: &User,
    username: &str,
    token: &str,
) -> (Vec<LegacyNotice>, Vec<String>) {
    use backend::{
        services::email::{mask_email, sessions_link, undo_link, EMAIL_CHANGE_UNDO_WINDOW_TEXT},
        templates::email::{
            email_changed_new_address_email, email_changed_old_address_email,
            email_in_use_notice_email, new_sign_in_email, welcome_email, RequestDetails,
        },
    };
    let es = &h.app.state.email_service;
    let app_url = es.app_url().to_string();
    let details = RequestDetails {
        client: Some("TestBrowser/1.0".into()),
        address: Some("102.89.x.x".into()),
        when_utc: "2026-11-17 12:00".into(),
    };
    let session = Uuid::new_v4();
    let taken = format!(
        "taken-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let new_address = format!(
        "nadia.new-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let support_address = format!(
        "support-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (change, support) = (Uuid::new_v4(), Uuid::new_v4());

    es.send_welcome_email(&user.email, username, user.id)
        .await
        .unwrap();
    es.send_new_sign_in_alert(&user.email, username, user.id, session, &details)
        .await
        .unwrap();
    es.send_email_in_use_notice(&taken).await.unwrap();
    let before = Utc::now();
    let txn = h.db().begin().await.unwrap();
    backend::services::email::email_changed_notices_on(
        &txn,
        &user.email,
        &new_address,
        username,
        user.id,
        change,
        &app_url,
        Some(token),
    )
    .await
    .unwrap();
    backend::services::email::email_changed_notices_on(
        &txn,
        &new_address,
        &support_address,
        username,
        user.id,
        support,
        &app_url,
        None,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let after = Utc::now();
    let minutes = |f: &dyn Fn(&str) -> backend::templates::email::EmailTemplate| {
        let a = before.format("%Y-%m-%d %H:%M").to_string();
        let b = after.format("%Y-%m-%d %H:%M").to_string();
        if a == b {
            vec![f(&a)]
        } else {
            vec![f(&a), f(&b)]
        }
    };
    let undo = undo_link(&app_url, token);
    let notices = vec![
        LegacyNotice {
            key: format!("welcome:{}", user.id),
            category: "AUTH_WELCOME",
            to: user.email.clone(),
            name: Some(username.into()),
            subject: "Welcome to CF Meet",
            renderings: vec![welcome_email(username, &app_url)],
            event_type: "account.welcome.requested",
        },
        LegacyNotice {
            key: format!("new-sign-in:{session}"),
            category: "NOTIFICATION",
            to: user.email.clone(),
            name: Some(username.into()),
            subject: "New sign-in to your CF Meet account",
            renderings: vec![new_sign_in_email(
                username,
                &details,
                &sessions_link(&app_url),
            )],
            event_type: "account.security.new-sign-in",
        },
        LegacyNotice {
            key: format!(
                "email-in-use:{}:{}",
                email_delivery::recipient_hash(&taken),
                before.format("%Y%m%d%H")
            ),
            category: "NOTIFICATION",
            to: taken.clone(),
            name: None,
            subject: "Email change attempt on CF Meet",
            renderings: vec![email_in_use_notice_email()],
            event_type: "account.security.email-in-use",
        },
        LegacyNotice {
            key: format!("email-changed:old:{change}"),
            category: "NOTIFICATION",
            to: user.email.clone(),
            name: Some(username.into()),
            subject: "Your CF Meet email address was changed",
            renderings: minutes(&|when| {
                email_changed_old_address_email(
                    username,
                    &mask_email(&new_address),
                    when,
                    Some(&undo),
                    EMAIL_CHANGE_UNDO_WINDOW_TEXT,
                )
            }),
            event_type: "account.security.email-changed",
        },
        LegacyNotice {
            key: format!("email-changed:new:{change}"),
            category: "NOTIFICATION",
            to: new_address.clone(),
            name: Some(username.into()),
            subject: "Your CF Meet email address was updated",
            renderings: vec![email_changed_new_address_email(username)],
            event_type: "account.security.email-changed",
        },
        LegacyNotice {
            key: format!("email-changed:old:{support}"),
            category: "NOTIFICATION",
            to: new_address.clone(),
            name: Some(username.into()),
            subject: "Your CF Meet email address was changed",
            renderings: minutes(&|when| {
                email_changed_old_address_email(
                    username,
                    &mask_email(&support_address),
                    when,
                    None,
                    EMAIL_CHANGE_UNDO_WINDOW_TEXT,
                )
            }),
            event_type: "account.security.email-changed",
        },
        LegacyNotice {
            key: format!("email-changed:new:{support}"),
            category: "NOTIFICATION",
            to: support_address.clone(),
            name: Some(username.into()),
            subject: "Your CF Meet email address was updated",
            renderings: vec![email_changed_new_address_email(username)],
            event_type: "account.security.email-changed",
        },
    ];
    // What must never be readable outside the sealed parts.
    let secrets = vec![
        user.email.clone(),
        taken,
        new_address.clone(),
        support_address,
        mask_email(&new_address),
        token.to_string(),
        "TestBrowser".to_string(),
        "102.89".to_string(),
    ];
    (notices, secrets)
}

/// Every account notice enqueue point (`ACCOUNT_NOTICES`): under FENCED_NEW each is decided
/// NEW with its catalogue event and `fallback_sealed` equal to the legacy email; the undo
/// token, the masked new address, every address and the request details appear nowhere in
/// the envelope's clear part, in the outbox row outside `sealed` / `fallback_sealed`, or in
/// the decision. Under LEGACY_ONLY each records exactly today's ledger row and nothing
/// else.
#[tokio::test]
async fn every_account_notice_enqueue_point_is_fenced() {
    let h = some!(H::new().await);
    let username = "ada";
    let token = "Undo0123456789abcdefghijklmnopqrstuvwxyzAB";
    for mode in ["LEGACY_ONLY", "FENCED_NEW"] {
        h.set_all(mode, "AUTO").await;
        let user = h
            .app
            .user(&format!("notices-{}", mode.to_ascii_lowercase()))
            .await;
        let (notices, secrets) = account_notices(&h, &user, username, token).await;
        assert_eq!(notices.len(), 7);
        for n in &notices {
            let key = &n.key;
            if mode == "LEGACY_ONLY" {
                assert_eq!(h.emails_for(key).await, 1, "{key}");
                assert_eq!(
                    h.count(&format!(
                        "SELECT count(*) FROM notification_route_decisions WHERE idempotency_key = '{key}'"
                    ))
                    .await,
                    0,
                    "LEGACY_ONLY records no decision: {key}"
                );
                let r = h
                    .db()
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT category, recipient, recipient_name, subject, text_body, html_body
                           FROM email_deliveries WHERE idempotency_key = $1",
                        [key.clone().into()],
                    ))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(r.try_get::<String>("", "category").unwrap(), n.category);
                assert_eq!(r.try_get::<String>("", "recipient").unwrap(), n.to);
                assert_eq!(
                    r.try_get::<Option<String>>("", "recipient_name").unwrap(),
                    n.name
                );
                assert_eq!(r.try_get::<String>("", "subject").unwrap(), n.subject);
                let (text, html): (String, String) = (
                    r.try_get("", "text_body").unwrap(),
                    r.try_get("", "html_body").unwrap(),
                );
                assert!(
                    n.renderings
                        .iter()
                        .any(|t| t.text == text && t.html == html),
                    "today's rendering, byte for byte: {key}"
                );
                continue;
            }
            let (id, route, reason, ..) = h.decision(key).await;
            assert_eq!(
                (route.as_str(), reason.as_str()),
                ("NEW", "ROUTED"),
                "{key}"
            );
            assert_eq!(h.emails_for(key).await, 0, "never both: {key}");
            let (state, shadow, event, _, _) = h.outbox(id).await;
            assert_eq!((state.as_str(), shadow), ("PENDING", false));
            assert_eq!(event["eventType"], n.event_type, "{key}");
            validate(
                &serde_json::from_value(event.clone()).unwrap(),
                Some(MEETING),
            )
            .unwrap();
            assert_eq!(
                event["recipients"][0]["ref"],
                owner::email_principal_ref(&n.to),
                "{key}"
            );
            assert_eq!(event["delivery"]["priority"], "CRITICAL");
            assert_eq!(event["delivery"]["idempotencyKey"], *key);
            assert!(event["sealed"]["ct"].is_string(), "{key}");
            match n.event_type {
                "account.security.email-in-use" => {
                    assert!(event.get("subject").is_none(), "never names the account")
                }
                _ => assert_eq!(event["subject"]["id"], json!(user.id), "{key}"),
            }
            match n.event_type {
                "account.security.new-sign-in" => {
                    assert_eq!(
                        event["data"],
                        json!({ "requestedAt": "2026-11-17T12:00:00Z" })
                    )
                }
                "account.security.email-changed" => {
                    let side = if key.starts_with("email-changed:old:") {
                        "OLD"
                    } else {
                        "NEW"
                    };
                    assert_eq!(event["data"]["side"], side);
                    assert!(event["data"]["changedAt"]
                        .as_str()
                        .unwrap()
                        .ends_with(":00Z"));
                    assert_eq!(event["data"].as_object().unwrap().len(), 2);
                }
                _ => assert_eq!(event["data"], json!({})),
            }
            // The fallback is exactly the LEGACY_ONLY email.
            let opened = h
                .keys
                .fallback
                .as_ref()
                .unwrap()
                .open(id, &h.fallback_blob(id).await)
                .unwrap();
            assert_eq!(opened.category, n.category, "{key}");
            assert_eq!(&opened.idempotency_key, key);
            assert_eq!(opened.to, n.to);
            assert_eq!(opened.to_name, n.name);
            assert_eq!(opened.subject, n.subject);
            assert!(
                n.renderings
                    .iter()
                    .any(|t| t.text == opened.text && t.html == opened.html),
                "{key}"
            );
            if key.starts_with("email-changed:old:") && opened.text.contains("undo the change") {
                assert!(opened
                    .text
                    .contains(&format!("/auth/email-change/undo?token={token}")));
            }
            // Nothing readable outside the sealed parts: the envelope's clear part, every
            // other column of the outbox row, the decision row.
            let row = h
                .db()
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT ((to_jsonb(o) - 'fallback_sealed' - 'event')
                             || jsonb_build_object('event', o.event - 'sealed'))::text AS clear,
                            (SELECT to_jsonb(d)::text FROM notification_route_decisions d
                              WHERE d.decision_id = o.decision_id) AS decision
                       FROM account_notification_outbox o WHERE o.decision_id = $1",
                    [id.into()],
                ))
                .await
                .unwrap()
                .unwrap();
            let clear: String = row.try_get("", "clear").unwrap();
            let decision: String = row.try_get("", "decision").unwrap();
            for secret in &secrets {
                assert!(!clear.contains(secret.as_str()), "{key}: {secret} in clear");
                assert!(
                    !decision.contains(secret.as_str()),
                    "{key}: {secret} in the decision"
                );
            }
            assert!(!clear.contains("***"), "{key}: a masked address in clear");
        }
    }
    // The fenced category is ACCOUNT_NOTICES; every envelope is valid at I1.
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE category = 'ACCOUNT_NOTICES' AND route = 'NEW'")
            .await,
        7
    );
    assert_eq!(h.relay(Utc::now()).await.acked, 7);
    assert!(h.i1.invalid().is_empty(), "{:?}", h.i1.invalid());
}

/// Account notices in FENCED_NEW / AUTO: the welcome is sent by the service under its X6
/// lease (X7 completes it; the legacy path never sends it); the email-changed notices are
/// never acknowledged, so AUTO fails them over after 60 s and the legacy worker sends the
/// identical legacy emails — the undo link included — exactly once, and the service never
/// can.
#[tokio::test]
async fn account_notices_go_new_and_fail_over_with_the_identical_email() {
    use backend::services::email::{email_changed_notices_on, undo_link};
    let h = some!(H::new().await);
    h.set_all("FENCED_NEW", "AUTO").await;
    let user = h.app.user("notices-fo").await;
    let es = &h.app.state.email_service;
    es.send_welcome_email(&user.email, "ada", user.id)
        .await
        .unwrap();
    let welcome_key = format!("welcome:{}", user.id);
    let (welcome_id, route, ..) = h.decision(&welcome_key).await;
    assert_eq!(route, "NEW");
    // A repeat is sticky: the same decision, nothing new.
    es.send_welcome_email(&user.email, "ada", user.id)
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM account_notification_outbox")
            .await,
        1
    );
    assert_eq!(h.relay(Utc::now()).await.acked, 1);
    assert_eq!(h.service_attempt(welcome_id).await, "GRANTED");
    assert!(h.decision(&welcome_key).await.6, "completed");

    // The email change: the service never acknowledges either notice.
    h.i1.down(Some(503));
    let new_address = format!(
        "moved-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let change = Uuid::new_v4();
    let token = "Undo0123456789abcdefghijklmnopqrstuvwxyzAB";
    let txn = h.db().begin().await.unwrap();
    email_changed_notices_on(
        &txn,
        &user.email,
        &new_address,
        "ada",
        user.id,
        change,
        es.app_url(),
        Some(token),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let old_key = format!("email-changed:old:{change}");
    let new_key = format!("email-changed:new:{change}");
    let (old_id, ..) = h.decision(&old_key).await;
    let (new_id, ..) = h.decision(&new_key).await;
    let key = h.keys.fallback.as_ref().unwrap();
    let old_legacy = key.open(old_id, &h.fallback_blob(old_id).await).unwrap();
    let new_legacy = key.open(new_id, &h.fallback_blob(new_id).await).unwrap();
    assert_eq!(h.relay(later(30)).await.failed_over, 0);
    assert_eq!(h.relay(later(61)).await.failed_over, 2);
    for key in [&old_key, &new_key] {
        let (_, route, reason, ..) = h.decision(key).await;
        assert_eq!((route.as_str(), reason.as_str()), ("OLD", "AUTO_FAILOVER"));
        assert_eq!(h.emails_for(key).await, 1);
    }
    // The secrets are erased from the settled rows.
    for id in [old_id, new_id] {
        let (_, _, event, len, settled) = h.outbox(id).await;
        assert!(settled && len == 0 && event.get("sealed").is_none());
    }
    h.i1.down(None);
    assert_eq!(h.service_attempt(old_id).await, "REVOKED");
    assert_eq!(h.service_attempt(new_id).await, "REVOKED");
    assert_eq!(h.relay(later(7200)).await.failed_over, 0);

    let provider = Arc::new(RecordingProvider::default());
    let worker = EmailWorker::new(h.app.state.db.clone(), provider.clone(), settings());
    worker.tick(Utc::now()).await.unwrap();
    worker.tick(Utc::now()).await.unwrap();
    let mut sent = provider.sent();
    sent.sort_by(|a, b| a.2.cmp(&b.2));
    assert_eq!(
        sent.len(),
        2,
        "only the failed-over notices reach the legacy path, once"
    );
    assert_eq!(
        (sent[0].1.as_str(), sent[0].2.as_str()),
        (
            user.email.as_str(),
            "Your CF Meet email address was changed"
        )
    );
    assert_eq!(
        sent[0].3, old_legacy.text,
        "the legacy email, byte for byte"
    );
    assert!(
        sent[0].3.contains(&undo_link(es.app_url(), token)),
        "the same undo link"
    );
    assert!(sent[0].3.contains("m***@example.com"));
    assert_eq!(
        (sent[1].1.as_str(), sent[1].2.as_str()),
        (
            new_address.as_str(),
            "Your CF Meet email address was updated"
        )
    );
    assert_eq!(sent[1].3, new_legacy.text);
    assert_eq!(h.service_sent(welcome_id), 1);
    assert_eq!((h.service_sent(old_id), h.service_sent(new_id)), (0, 0));
    assert_eq!(h.emails_for(&welcome_key).await, 0);
}

/// A suppressed recipient: the welcome (`AUTH_WELCOME`, suppressible today) is refused in
/// a fenced category exactly as `LEGACY_ONLY` refuses it, and nothing is decided.
#[tokio::test]
async fn a_suppressed_welcome_is_refused_as_today() {
    let h = some!(H::new().await);
    let user = h.app.user("suppressed").await;
    h.exec(
        "INSERT INTO email_deliveries (id, category, idempotency_key, recipient_hash, subject, status)
         VALUES ($1, 'NOTIFICATION', $2, $3, 's', 'COMPLAINED')",
        vec![
            Uuid::new_v4().into(),
            format!("complaint:{}", user.id).into(),
            email_delivery::recipient_hash(&user.email).into(),
        ],
    )
    .await;
    for mode in ["LEGACY_ONLY", "FENCED_NEW"] {
        h.set_all(mode, "AUTO").await;
        let err = h
            .app
            .state
            .email_service
            .send_welcome_email(&user.email, "ada", user.id)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains(email_delivery::RECIPIENT_SUPPRESSED),
            "{mode}: {err}"
        );
    }
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions")
            .await,
        0
    );
    assert_eq!(h.emails_for(&format!("welcome:{}", user.id)).await, 0);
}
