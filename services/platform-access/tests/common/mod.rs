//! Shared harness for Platform Access integration tests.
//!
//! Requires PostgreSQL (no Docker needed): `TEST_DATABASE_URL` is an admin URL, e.g.
//! `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Each test gets its own database
//! cloned from a freshly migrated template and dropped afterwards. Without
//! `TEST_DATABASE_URL` the tests FAIL unless `CFMEET_SKIP_DB_TESTS=1` is set (then they
//! print an explicit SKIPPED line — never a silent pass).
//!
//! Requests go through the real router (`http::app`) with real Ed25519 service tokens
//! from independent peer identities, and a manually driven clock.
#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, KeyId, OutboundServiceRequest,
    RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use platform_access::{
    config::{service_auth::ServiceAuthRuntime, CredentialPolicy},
    domain::{EnvironmentKind, ManualClock},
    http,
    identity::{
        authentication::CredentialAuthenticator,
        scopes::ScopeRegistry,
        verifier::{CredentialPepper, VerifierKeyring},
        PlatformService,
    },
    state::AppState,
};
use platform_access_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

/// Deterministic test-only pepper (never used outside tests).
pub const TEST_PEPPER: &str = "platform-access-test-pepper-7d2e9f0a1b3c5d7e9f1a3b5c7d";
pub const OPERATOR: &str = "ops.tester";

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

pub fn admin_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!("SKIPPED: database integration test (CFMEET_SKIP_DB_TESTS=1, TEST_DATABASE_URL unset)");
                None
            } else {
                panic!(
                    "TEST_DATABASE_URL is not set. Platform Access integration tests require PostgreSQL \
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

async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    if !std::path::Path::new("/proc/self").exists() {
        return;
    }
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'pa\\_t\\_%' OR datname LIKE 'pa\\_tmpl\\_%'",
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
            .trim_start_matches("pa_tmpl_")
            .trim_start_matches("pa_t_")
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
            let name = format!("pa_tmpl_{}_{}", std::process::id(), Uuid::new_v4().simple());
            let conn = Database::connect(admin).await.expect("connect admin db");
            drop_databases_of_dead_runs(&conn).await;
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            platform_access_migration::Migrator::up(&tmpl, None)
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
    name: String,
    admin: String,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        let name = format!("pa_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = CREATE_LOCK.lock().await;
            let conn = Database::connect(&admin).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        Some(Self::connect(admin, name).await)
    }

    /// Empty database (no migrations), for migration tests.
    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        let name = format!("pa_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        let conn = Database::connect(&admin).await.unwrap();
        conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        conn.close().await.ok();
        Some(Self::connect(admin, name).await)
    }

    async fn connect(admin: String, name: String) -> Self {
        let url = with_db(&admin, &name);
        let mut opts = sea_orm::ConnectOptions::new(url.clone());
        opts.max_connections(10).sqlx_logging(false);
        let conn = Database::connect(opts).await.unwrap();
        Self {
            conn,
            url,
            name,
            admin,
        }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                if let Ok(conn) = Database::connect(&admin).await {
                    let _ = conn
                        .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                        .await;
                    conn.close().await.ok();
                }
            });
        })
        .join();
    }
}

pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2031-03-01T10:00:30Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// Fixed service-auth time, 2031-03-01T10:00:00Z.
pub const SERVICE_T0: i64 = 1_930_212_000;

pub fn keyring(pepper: &str) -> VerifierKeyring {
    VerifierKeyring::new(
        1,
        CredentialPepper::parse("API_CREDENTIAL_PEPPER", pepper).unwrap(),
        None,
    )
    .unwrap()
}

fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

/// Independent peers that call Platform Access in tests.
pub struct Peers {
    pub clock: cf_service_auth::ManualClock,
    pub calling: Arc<ServiceTokenSigner>,
    pub numbering: Arc<ServiceTokenSigner>,
    pub messaging: Arc<ServiceTokenSigner>,
    /// Trusted Messaging identity name, signing with a key the bundle does not list.
    pub rogue_messaging: Arc<ServiceTokenSigner>,
    pub operator: Arc<ServiceTokenSigner>,
    /// Correctly signing, but not in Platform Access's trust bundle.
    pub untrusted: Arc<ServiceTokenSigner>,
    /// Trusted Calling identity name, but signing with a key the bundle does not list.
    pub rogue_calling: Arc<ServiceTokenSigner>,
}

pub fn trust_bundle(
    receiver: &str,
    grants: &[(&str, Vec<cf_service_auth::PublicJwk>, &[&str])],
) -> String {
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": "test",
        "trusted_issuers": grants.iter().map(|(issuer, keys, permissions)| json!({
            "issuer": issuer,
            "audience": receiver,
            "runtime_environment": "test",
            "keys": keys,
            "permissions": permissions,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

pub const CALLING_PERMISSIONS: &[&str] = &[
    "platform-credential.authenticate",
    "platform-principal.resolve",
    "platform-principal.register",
    "platform-identity.read",
];
/// Part V B: what Messaging's grant in Platform Access's trust bundle holds.
pub const MESSAGING_PERMISSIONS: &[&str] = &[
    "platform-credential.authenticate",
    "platform-principal.resolve",
    "platform-identity.read",
];
pub const NUMBERING_PERMISSIONS: &[&str] = &[
    "platform-credential.authenticate",
    "platform-principal.resolve",
    "platform-identity.read",
];

pub fn service_auth() -> (ServiceAuthRuntime, Peers) {
    let env = RuntimeEnvironment::Test;
    let clock = cf_service_auth::ManualClock::new(SERVICE_T0);
    let shared: cf_service_auth::SharedClock = Arc::new(clock.clone());
    let me = key("platform-access-test-1");
    let calling = key("calling-test-1");
    let numbering = key("numbering-test-1");
    let messaging = key("messaging-test-1");
    let operator = key("operator-test-1");
    let policy = TrustPolicy::from_json(
        &trust_bundle(
            cf_service_auth::services::PLATFORM_ACCESS,
            &[
                (
                    cf_service_auth::services::CALLING,
                    vec![calling.public_jwk()],
                    CALLING_PERMISSIONS,
                ),
                (
                    cf_service_auth::services::NUMBERING,
                    vec![numbering.public_jwk()],
                    NUMBERING_PERMISSIONS,
                ),
                (
                    cf_service_auth::services::MESSAGING,
                    vec![messaging.public_jwk()],
                    MESSAGING_PERMISSIONS,
                ),
                (
                    cf_service_auth::services::OPERATOR_CONSOLE,
                    vec![operator.public_jwk()],
                    &["platform-identity.administer"],
                ),
            ],
        ),
        &ServiceId::platform_access(),
        env,
    )
    .unwrap();
    let signer = |id: ServiceId, k: ServiceSigningKey| {
        Arc::new(ServiceTokenSigner::new(
            id,
            env,
            k,
            TokenTtl::default(),
            shared.clone(),
        ))
    };
    let runtime = ServiceAuthRuntime::new(
        signer(ServiceId::platform_access(), me),
        Arc::new(ServiceTokenVerifier::new(policy, shared.clone())),
    )
    .unwrap();
    let peers = Peers {
        calling: signer(ServiceId::calling(), calling),
        numbering: signer(ServiceId::numbering(), numbering),
        messaging: signer(ServiceId::messaging(), messaging),
        rogue_messaging: signer(ServiceId::messaging(), key("messaging-rogue-1")),
        operator: signer(ServiceId::operator_console(), operator),
        untrusted: signer(ServiceId::telephony(), key("telephony-test-1")),
        rogue_calling: signer(ServiceId::calling(), key("calling-rogue-1")),
        clock,
    };
    (runtime, peers)
}

pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    pub state: AppState,
    pub app: Router,
    pub peers: Peers,
}

/// Response of a harness call.
#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub body: Value,
    pub headers: axum::http::HeaderMap,
}

impl Reply {
    pub fn code(&self) -> &str {
        self.body["error"]["code"].as_str().unwrap_or("")
    }
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with_keyring(keyring(TEST_PEPPER)).await
    }

    pub async fn with_keyring(ring: VerifierKeyring) -> Option<Self> {
        let db = TestDb::new().await?;
        Some(Self::on(Arc::new(db), ring))
    }

    /// Another deployment over the same database (e.g. a different keyring).
    pub fn on(db: Arc<TestDb>, ring: VerifierKeyring) -> Self {
        let clock = ManualClock::new(t0());
        let ring = Arc::new(ring);
        let registry = Arc::new(ScopeRegistry::standard());
        let platform = PlatformService::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            ring.clone(),
            registry.clone(),
            CredentialPolicy::default(),
        );
        let authenticator =
            CredentialAuthenticator::new(db.conn.clone(), ring, registry, Arc::new(clock.clone()));
        let (service_auth, peers) = service_auth();
        let state = AppState::new(platform, authenticator, service_auth);
        let app = http::app(state.clone());
        Self {
            db,
            clock,
            state,
            app,
            peers,
        }
    }

    pub fn conn(&self) -> &DatabaseConnection {
        &self.db.conn
    }

    /// A raw request (no service token unless `headers` add one).
    pub async fn raw(&self, request: Request<Body>) -> Reply {
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        Reply {
            status,
            body,
            headers,
        }
    }

    /// A signed internal request from `signer`, optionally with a delegated context and
    /// an operator header.
    pub async fn call(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        delegated: Option<DelegatedPlatformContext>,
        operator: Option<&str>,
    ) -> Reply {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        if let Some(ctx) = delegated {
            outbound = outbound.with_delegated_context(ctx);
        }
        let signed = signer.sign(&outbound).unwrap();
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in signed.headers().iter() {
            builder = builder.header(name, value);
        }
        if let Some(op) = operator {
            builder = builder.header("x-cf-operator", op);
        }
        let request = match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        self.raw(request).await
    }

    /// Operator administration call (operator-console identity + operator header).
    pub async fn admin(&self, method: Method, path: &str, body: Option<Value>) -> Reply {
        self.call(
            &self.peers.operator,
            method,
            path,
            body,
            None,
            Some(OPERATOR),
        )
        .await
    }

    pub async fn admin_ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let r = self.admin(method, path, body).await;
        assert!(r.status.is_success(), "{path}: {} {}", r.status, r.body);
        r.body
    }

    /// Calling-identity call with an optional delegated context.
    pub async fn as_calling(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        delegated: Option<DelegatedPlatformContext>,
    ) -> Reply {
        self.call(&self.peers.calling, method, path, body, delegated, None)
            .await
    }

    pub async fn authenticate(&self, credential: &str) -> Reply {
        self.as_calling(
            Method::POST,
            "/internal/v1/credentials/authenticate",
            Some(json!({ "credential": credential })),
            None,
        )
        .await
    }

    /// Creates tenant + application + all environments + a `backend` service account each
    /// and grants `scopes` to the application.
    pub async fn consumer(&self, slug: &str, classification: &str, scopes: &[&str]) -> Consumer {
        let tenant = self
            .admin_ok(
                Method::POST,
                "/internal/v1/admin/tenants",
                Some(json!({ "slug": slug, "displayName": format!("{slug} tenant"), "classification": classification })),
            )
            .await;
        let tenant_id = uuid(&tenant["id"]);
        let app = self
            .admin_ok(
                Method::POST,
                &format!("/internal/v1/admin/tenants/{tenant_id}/applications"),
                Some(json!({ "slug": "app", "displayName": "App" })),
            )
            .await;
        let application_id = uuid(&app["id"]);
        for scope in scopes {
            self.admin_ok(
                Method::POST,
                &format!("/internal/v1/admin/applications/{application_id}/scopes"),
                Some(json!({ "scope": scope })),
            )
            .await;
        }
        let mut environments = Vec::new();
        for kind in ["SANDBOX", "TEST", "PRODUCTION"] {
            let env = self
                .admin_ok(
                    Method::POST,
                    &format!("/internal/v1/admin/applications/{application_id}/environments"),
                    Some(json!({ "kind": kind })),
                )
                .await;
            let env_id = uuid(&env["id"]);
            let sa = self
                .admin_ok(
                    Method::POST,
                    &format!("/internal/v1/admin/environments/{env_id}/service-accounts"),
                    Some(json!({ "name": "backend" })),
                )
                .await;
            environments.push((kind.to_string(), env_id, uuid(&sa["id"])));
        }
        Consumer {
            tenant_id,
            application_id,
            environments,
        }
    }

    /// Issues a credential; returns (credential view, raw secret).
    pub async fn issue(&self, service_account: Uuid, scopes: &[&str]) -> (Value, String) {
        let body = self
            .admin_ok(
                Method::POST,
                &format!("/internal/v1/admin/service-accounts/{service_account}/credentials"),
                Some(json!({ "scopes": scopes })),
            )
            .await;
        let secret = body["secret"].as_str().unwrap().to_string();
        (body["credential"].clone(), secret)
    }
}

pub fn uuid(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap_or_else(|| panic!("not a uuid: {v}"))).unwrap()
}

pub struct Consumer {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    /// (kind, environment id, `backend` service account id)
    pub environments: Vec<(String, Uuid, Uuid)>,
}

impl Consumer {
    pub fn env(&self, kind: &str) -> Uuid {
        self.environments.iter().find(|e| e.0 == kind).unwrap().1
    }

    pub fn sa(&self, kind: &str) -> Uuid {
        self.environments.iter().find(|e| e.0 == kind).unwrap().2
    }

    pub fn delegated(&self, kind: &str) -> DelegatedPlatformContext {
        let k = match kind {
            "SANDBOX" => ApplicationEnvironmentKind::Sandbox,
            "TEST" => ApplicationEnvironmentKind::Test,
            _ => ApplicationEnvironmentKind::Production,
        };
        DelegatedPlatformContext::new(
            self.tenant_id,
            self.application_id,
            self.env(kind),
            k,
            self.sa(kind),
        )
    }

    pub fn kind(kind: &str) -> EnvironmentKind {
        EnvironmentKind::parse(kind).unwrap()
    }
}

/// The Part III verifier, computed INDEPENDENTLY of Platform Access's code: HMAC-SHA-256
/// under `pepper` of `label 0 tag 0 public_id 0 secret` (Calling's original algorithm).
pub fn part_iii_verifier(pepper: &str, tag: &str, public_id: &str, secret: &[u8; 32]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(pepper.as_bytes()).unwrap();
    mac.update(b"cfmeet.calling.api-credential.v1");
    mac.update(&[0]);
    mac.update(tag.as_bytes());
    mac.update(&[0]);
    mac.update(public_id.as_bytes());
    mac.update(&[0]);
    mac.update(secret);
    mac.finalize().into_bytes().to_vec()
}
