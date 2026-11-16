//! Shared harness for Notification Service integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets its
//! own database cloned from a freshly migrated template. Without it the tests FAIL, unless
//! `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED line).
//!
//! Requests go through the REAL router with REAL Ed25519 service tokens verified by the
//! service's trust bundle; providers are the recording email provider and the local-test
//! push adapter; owners are the in-memory `StaticAuthorities`.
#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    Router,
};
use cf_notification_client::{
    seal::{generate_keypair, OpeningKey},
    EnvelopeBuilder, Producer, RouteRef, SealedPlaintext, SealingKey,
};
use cf_push_transport::{LocalTestPush, PushProvider};
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use http_body_util::BodyExt;
use notifications::{
    authorities::StaticAuthorities,
    config::EmailConfig,
    crypto::DataKeys,
    email::{EmailWorker, RecordingProvider},
    http,
    metrics::{Heartbeats, Metrics},
    processor::Processor,
    state::AppState,
};
use notifications_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

pub const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
pub const HPKE_KID: &str = "notif-hpke-2026-11-test";

pub fn admin_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!("SKIPPED: database integration test (CFMEET_SKIP_DB_TESTS=1, TEST_DATABASE_URL unset)");
                None
            } else {
                panic!(
                    "TEST_DATABASE_URL is not set. Notifications integration tests require PostgreSQL \
                     (e.g. TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres). \
                     Set CFMEET_SKIP_DB_TESTS=1 to skip explicitly."
                )
            }
        }
    }
}

pub fn with_db(admin: &str, db: &str) -> String {
    let (base, _) = admin
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL must include a database name");
    format!("{base}/{db}")
}

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    if !std::path::Path::new("/proc/self").exists() {
        return;
    }
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'notif\\_t\\_%' OR datname LIKE 'notif\\_tmpl\\_%'",
        ))
        .await
    else {
        return;
    };
    for row in rows {
        let Ok(name) = row.try_get_by_index::<String>(0) else {
            continue;
        };
        let pid = name
            .trim_start_matches("notif_tmpl_")
            .trim_start_matches("notif_t_")
            .split('_')
            .next()
            .and_then(|p| p.parse::<u32>().ok());
        match pid {
            Some(pid) if std::path::Path::new(&format!("/proc/{pid}")).exists() => {}
            _ => {
                let _ = conn
                    .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                    .await;
            }
        }
    }
}

async fn template(admin: &str) -> String {
    TEMPLATE
        .get_or_init(|| async {
            let name = format!(
                "notif_tmpl_{}_{}",
                std::process::id(),
                Uuid::new_v4().simple()
            );
            let conn = Database::connect(admin).await.expect("connect admin db");
            drop_databases_of_dead_runs(&conn).await;
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            notifications_migration::Migrator::up(&tmpl, None)
                .await
                .expect("migrations apply");
            tmpl.close().await.ok();
            name
        })
        .await
        .clone()
}

/// An isolated database for one test, dropped (FORCE) on drop.
pub struct TestDb {
    pub conn: DatabaseConnection,
    pub url: String,
    admin: String,
    name: String,
}

impl TestDb {
    /// A migrated database.
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        Some(Self::create(admin, Some(tmpl)).await)
    }

    /// An empty database (migration tests).
    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        Some(Self::create(admin, None).await)
    }

    async fn create(admin: String, template: Option<String>) -> Self {
        let name = format!("notif_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = CREATE_LOCK.lock().await;
            let conn = Database::connect(&admin).await.expect("connect admin db");
            let sql = match &template {
                Some(t) => format!("CREATE DATABASE {name} TEMPLATE {t}"),
                None => format!("CREATE DATABASE {name}"),
            };
            conn.execute_unprepared(&sql).await.expect("create test db");
            conn.close().await.ok();
        }
        let url = with_db(&admin, &name);
        let conn = Database::connect(&url).await.unwrap();
        Self {
            conn,
            url,
            admin,
            name,
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                if let Ok(conn) = Database::connect(&admin).await {
                    let _ = conn
                        .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                        .await;
                }
            });
        })
        .join()
        .ok();
    }
}

pub fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

fn signer(id: ServiceId, k: ServiceSigningKey) -> Arc<ServiceTokenSigner> {
    Arc::new(ServiceTokenSigner::new(
        id,
        ENV,
        k,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ))
}

pub struct Peers {
    pub meeting: Arc<ServiceTokenSigner>,
    pub organizations: Arc<ServiceTokenSigner>,
    pub communications: Arc<ServiceTokenSigner>,
    pub calling: Arc<ServiceTokenSigner>,
    pub messaging: Arc<ServiceTokenSigner>,
    pub developer: Arc<ServiceTokenSigner>,
    pub operator: Arc<ServiceTokenSigner>,
    pub rogue_meeting: Arc<ServiceTokenSigner>,
}

pub fn verifier_and_peers() -> (Arc<ServiceTokenVerifier>, Peers) {
    use cf_service_auth::services as s;
    let clock: SharedClock = Arc::new(SystemClock);
    let keys: Vec<(&str, ServiceSigningKey)> = [
        "meeting",
        "organizations",
        "communications",
        "calling",
        "messaging",
        "developer",
        "operator",
    ]
    .iter()
    .map(|n| (*n, key(&format!("{n}-test-1"))))
    .collect();
    let k = |n: &str| keys.iter().find(|(x, _)| *x == n).map(|(_, k)| k).unwrap();
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| json!({ "issuer": id, "audience": s::NOTIFICATIONS, "runtime_environment": ENV, "keys": [k.public_jwk()], "permissions": perms });
    let bundle = json!({
        "bundle_version": 1,
        "receiver": s::NOTIFICATIONS,
        "runtime_environment": ENV,
        "trusted_issuers": [
            issuer(s::MEETING_SERVICE, k("meeting"), &["notifications-event.publish", "notifications-user.forward"]),
            issuer(s::ORGANIZATIONS, k("organizations"), &["notifications-event.publish"]),
            issuer(s::COMMUNICATIONS, k("communications"), &["notifications-event.publish"]),
            issuer(s::CALLING, k("calling"), &["notifications-event.publish", "notifications-call-push.dispatch"]),
            issuer(s::MESSAGING, k("messaging"), &["notifications-event.publish"]),
            issuer(s::DEVELOPER_PLATFORM, k("developer"), &["notifications-event.publish"]),
            issuer(s::OPERATOR_CONSOLE, k("operator"), &["notifications-observability.read", "notifications-operations.replay", "notifications-operations.reroute"]),
        ],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::notifications(), ENV).unwrap();
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock));
    let take = |n: &str| {
        keys.iter()
            .find(|(x, _)| *x == n)
            .map(|(_, k)| {
                ServiceSigningKey::from_pkcs8_pem(
                    KeyId::parse(&format!("{n}-test-1")).unwrap(),
                    &k.to_pkcs8_pem(),
                )
                .unwrap()
            })
            .unwrap()
    };
    (
        verifier,
        Peers {
            meeting: signer(ServiceId::meeting_service(), take("meeting")),
            organizations: signer(ServiceId::organizations(), take("organizations")),
            communications: signer(ServiceId::communications(), take("communications")),
            calling: signer(ServiceId::calling(), take("calling")),
            messaging: signer(ServiceId::messaging(), take("messaging")),
            developer: signer(
                ServiceId::parse(s::DEVELOPER_PLATFORM).unwrap(),
                take("developer"),
            ),
            operator: signer(ServiceId::operator_console(), take("operator")),
            rogue_meeting: signer(ServiceId::meeting_service(), key("meeting-test-1")),
        },
    )
}

#[derive(Debug)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
}

impl Response {
    pub fn code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }

    #[track_caller]
    pub fn expect(&self, status: u16) -> &Value {
        assert_eq!(
            self.status.as_u16(),
            status,
            "unexpected response: {:#}",
            self.json
        );
        &self.json
    }

    #[track_caller]
    pub fn expect_error(&self, status: u16, code: &str) {
        assert_eq!(
            (self.status.as_u16(), self.code()),
            (status, code),
            "unexpected response: {:#}",
            self.json
        );
    }
}

pub struct Harness {
    pub db: TestDb,
    pub router: Router,
    pub peers: Peers,
    pub state: AppState,
    pub provider: Arc<RecordingProvider>,
    pub push: Arc<LocalTestPush>,
    pub owners: Arc<StaticAuthorities>,
    pub sealing: SealingKey,
}

pub fn test_keys() -> DataKeys {
    let raw: Vec<u8> = (0u8..32)
        .map(|i| i.wrapping_mul(37).wrapping_add(11))
        .collect();
    DataKeys::new(("test-1", &raw), None).unwrap()
}

pub fn alice() -> String {
    "cfmeet-user:11111111-1111-4111-8111-111111111111".into()
}
pub fn bob() -> String {
    "cfmeet-user:22222222-2222-4222-8222-222222222222".into()
}
pub const ALICE_EMAIL: &str = "alice@example.com";
pub const BOB_EMAIL: &str = "bob@example.com";

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with_email(EmailConfig::disabled()).await
    }

    pub async fn with_email(email: EmailConfig) -> Option<Self> {
        let db = TestDb::new().await?;
        let (verifier, peers) = verifier_and_peers();
        let (public, private) = generate_keypair(HPKE_KID).unwrap();
        let opening = OpeningKey::from_bytes(HPKE_KID, &private).unwrap();
        let provider = Arc::new(RecordingProvider::default());
        let push = Arc::new(LocalTestPush::default());
        let owners = Arc::new(
            StaticAuthorities::default()
                .with_contact(&alice(), ALICE_EMAIL, "Alice")
                .with_contact(&bob(), BOB_EMAIL, "Bob"),
        );
        let state = AppState {
            db: db.conn.clone(),
            verifier,
            keys: Arc::new(test_keys()),
            opening: Some(Arc::new(opening)),
            email: Arc::new(email),
            email_provider: Some(provider.clone()),
            push: Some(push.clone() as Arc<dyn PushProvider>),
            authorities: owners.clone(),
            metrics: Arc::new(Metrics::default()),
            heartbeats: Arc::new(Heartbeats::default()),
            replica_id: "test-replica".into(),
            runtime_environment: ENV,
            workers_enabled: false,
            operator_emails: vec!["ops@example.com".into()],
            operator_alert_token: None,
        };
        Some(Self {
            router: http::app(state.clone()),
            db,
            peers,
            state,
            provider,
            push,
            owners,
            sealing: SealingKey::parse(&public).unwrap(),
        })
    }

    pub fn processor(&self) -> Processor {
        self.state.processor()
    }

    pub fn email_worker(&self, lane: &'static str) -> EmailWorker {
        EmailWorker::new(
            self.state.db.clone(),
            self.state.keys.clone(),
            self.provider.clone(),
            self.state.authorities.clone(),
            self.state.email.clone(),
            self.state.metrics.clone(),
            lane,
            "test",
        )
    }

    pub fn push_worker(&self) -> notifications::push::PushWorker {
        notifications::push::PushWorker::new(
            self.state.db.clone(),
            self.state.keys.clone(),
            self.push.clone(),
            self.state.metrics.clone(),
        )
    }

    /// Runs W1 until no event is left.
    pub async fn process_all(&self) {
        let p = self.processor();
        for _ in 0..20 {
            if p.tick(chrono::Utc::now()).await.unwrap() == 0 {
                return;
            }
        }
    }

    pub async fn set_mode(&self, category: &str, channel: &str, mode: &str) {
        self.exec(&format!(
            "UPDATE notif_routing SET mode = '{mode}', version = version + 1, changed_by_run = 'test' WHERE category = '{category}' AND channel = '{channel}'"
        ))
        .await
        .unwrap();
    }

    pub async fn set_category(&self, category: &str, mode: &str) {
        self.exec(&format!(
            "UPDATE notif_routing SET mode = '{mode}', version = version + 1, changed_by_run = 'test' WHERE category = '{category}'"
        ))
        .await
        .unwrap();
    }

    pub async fn signed(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Response {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::notifications(),
            ))
            .unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        for (k, v) in extra {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        self.raw(method, path, headers, body).await
    }

    pub async fn ingest(&self, signer: &ServiceTokenSigner, events: Vec<Value>) -> Response {
        self.signed(
            signer,
            Method::POST,
            "/internal/v1/notification-events",
            Some(json!({ "events": events })),
            &[],
        )
        .await
    }

    /// A U1–U9 call relayed by the Meeting Service for `account`.
    pub async fn user(
        &self,
        account: &str,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut d = json!({ "method": method, "path": path, "account": account, "idempotencyKey": format!("idem-{}", Uuid::new_v4().simple()) });
        if let Some(q) = query {
            d["query"] = json!(q);
        }
        if let Some(b) = body {
            d["body"] = b;
        }
        let peer = self.peers.meeting.clone();
        self.signed(
            &peer,
            Method::POST,
            "/internal/v1/notifications/dispatch",
            Some(d),
            &[],
        )
        .await
    }

    pub async fn raw(
        &self,
        method: Method,
        path: &str,
        headers: HeaderMap,
        body: Option<Value>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        let body = match body {
            Some(v) => {
                request = request.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Response {
            status,
            headers,
            json: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    pub async fn count(&self, sql: &str) -> i64 {
        self.db
            .conn
            .query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<i64>(0)
            .unwrap()
    }

    pub async fn text(&self, sql: &str) -> Option<String> {
        self.db
            .conn
            .query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
            .await
            .unwrap()
            .and_then(|r| r.try_get_by_index::<Option<String>>(0).ok().flatten())
    }

    pub async fn exec(&self, sql: &str) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
        self.db.conn.execute_unprepared(sql).await
    }
}

pub fn route() -> RouteRef {
    RouteRef {
        decision_id: Uuid::new_v4(),
        routing_version: 1,
        protocol: 1,
    }
}

pub const NOW: &str = "2026-11-16T12:00:00Z";

/// A valid envelope as JSON.
pub fn envelope(
    producer: Producer,
    event_type: &str,
    recipients: &[String],
    data: &[(&str, Value)],
    idem: Option<&str>,
) -> Value {
    let mut b = EnvelopeBuilder::new(producer, event_type, NOW, route())
        .unwrap()
        .dedupe_key(format!("{event_type}:{}", Uuid::new_v4().simple()));
    for r in recipients {
        b = b.recipient(r.clone());
    }
    for (k, v) in data {
        b = b.data(k, v.clone());
    }
    if let Some(i) = idem {
        b = b.idempotency_key(i);
    }
    serde_json::to_value(b.build().unwrap()).unwrap()
}

/// A sealed envelope (codes, tokens, invitee addresses).
pub fn sealed_envelope(
    key: &SealingKey,
    producer: Producer,
    event_type: &str,
    recipients: &[String],
    data: &[(&str, Value)],
    idem: &str,
    secret: SealedPlaintext,
) -> Value {
    let mut b = EnvelopeBuilder::new(producer, event_type, NOW, route())
        .unwrap()
        .dedupe_key(format!("{event_type}:{}", Uuid::new_v4().simple()))
        .idempotency_key(idem);
    for r in recipients {
        b = b.recipient(r.clone());
    }
    for (k, v) in data {
        b = b.data(k, v.clone());
    }
    serde_json::to_value(b.secret(secret).build_sealed(key).unwrap()).unwrap()
}
