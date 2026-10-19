//! Shared harness for Messaging integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets
//! its own database cloned from a freshly migrated template. Without it the tests FAIL,
//! unless `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED line).
//!
//! Requests go through the REAL router. Customer credentials are resolved by a scripted
//! Platform Access (the real Platform Access process is exercised by
//! `tests/platform_access_e2e.rs`); devices are REAL OpenMLS clients
//! (`cf-messaging-crypto` with its `client` feature — test code only).
#![allow(dead_code)]

pub mod flows;
pub mod gate;
pub mod realtime;

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex as StdMutex},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_messaging_crypto::device::Device;
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use messaging::{
    config::{service_auth::ServiceAuthRuntime, DeviceSettings},
    domain::ManualClock,
    http,
    identity::{
        cache::CachingAuthority, Authentication, AuthorityUnavailable, CustomerAuthority,
        EnvironmentKind, OwnerScope, PrincipalDirectory, PrincipalStatus, TrustedContext,
    },
    state::AppState,
};
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

pub const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

pub fn admin_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!("SKIPPED: database integration test (CFMEET_SKIP_DB_TESTS=1, TEST_DATABASE_URL unset)");
                None
            } else {
                panic!(
                    "TEST_DATABASE_URL is not set. Messaging integration tests require PostgreSQL \
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
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'msg\\_t\\_%' OR datname LIKE 'msg\\_tmpl\\_%'",
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
            .trim_start_matches("msg_tmpl_")
            .trim_start_matches("msg_t_")
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
                "msg_tmpl_{}_{}",
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
            messaging_migration::Migrator::up(&tmpl, None)
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
    pub name: String,
    admin: String,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        Some(Self::create(admin, Some(tmpl)).await)
    }

    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        Some(Self::create(admin, None).await)
    }

    async fn create(admin: String, template: Option<String>) -> Self {
        let name = format!("msg_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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
            name,
            admin,
        }
    }

    pub fn admin(&self) -> &str {
        &self.admin
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

pub fn t0() -> DateTime<Utc> {
    // The harness clock follows real time: OpenMLS checks KeyPackage lifetimes on the
    // system clock, so the server clock must agree with it.
    Utc::now()
}

// ---------------------------------------------------------------------------------------
// Scripted Platform Access
// ---------------------------------------------------------------------------------------

#[derive(Default)]
pub struct FakeAuthority {
    pub tokens: StdMutex<HashMap<String, TrustedContext>>,
    pub down: StdMutex<bool>,
    pub calls: StdMutex<usize>,
    /// Delegated scopes Platform Access recognizes: environment -> (scope, first party,
    /// full authority).
    pub delegated: StdMutex<HashMap<Uuid, (OwnerScope, bool, bool)>>,
}

#[async_trait]
impl CustomerAuthority for FakeAuthority {
    async fn authenticate(&self, presented: &str) -> Result<Authentication, AuthorityUnavailable> {
        *self.calls.lock().unwrap() += 1;
        if *self.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        Ok(match self.tokens.lock().unwrap().get(presented) {
            Some(c) => Authentication::Authenticated(Box::new(c.clone())),
            None => Authentication::Rejected {
                reason: "unknown".into(),
                throttle: None,
            },
        })
    }

    async fn validate_delegated(
        &self,
        ctx: &cf_service_auth::DelegatedPlatformContext,
    ) -> Result<messaging::identity::DelegatedScope, AuthorityUnavailable> {
        if *self.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        let known = self
            .delegated
            .lock()
            .unwrap()
            .get(&ctx.application_environment_id())
            .cloned();
        Ok(match known {
            Some((scope, first_party, full_authority))
                if scope.tenant_id == ctx.tenant_id()
                    && scope.application_id == ctx.application_id()
                    && scope.service_account_id == ctx.initiating_service_account_id() =>
            {
                messaging::identity::DelegatedScope::Valid {
                    first_party,
                    full_authority,
                }
            }
            _ => messaging::identity::DelegatedScope::Invalid,
        })
    }
}

/// Principals per environment: (environment, principal) -> status.
#[derive(Default)]
pub struct FakePrincipals {
    pub known: StdMutex<HashMap<(Uuid, Uuid), PrincipalStatus>>,
    pub down: StdMutex<bool>,
}

#[async_trait]
impl PrincipalDirectory for FakePrincipals {
    async fn resolve(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        if *self.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        Ok(self
            .known
            .lock()
            .unwrap()
            .get(&(scope.environment_id, principal_id))
            .cloned()
            .unwrap_or(PrincipalStatus::NotFound))
    }
}

/// One customer environment and a credential for it.
#[derive(Clone)]
pub struct Customer {
    pub token: String,
    pub context: TrustedContext,
}

// ---------------------------------------------------------------------------------------
// Service identities
// ---------------------------------------------------------------------------------------

pub fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

/// Signing identities of Messaging's peers (Part V H): each trusted with exactly its
/// own permissions, plus untrusted / rogue signers.
pub struct Peers {
    pub meeting: Arc<ServiceTokenSigner>,
    pub privacy_relay: Arc<ServiceTokenSigner>,
    pub sms_gateway: Arc<ServiceTokenSigner>,
    /// The Meeting Service's name with a key Messaging's bundle does not list.
    pub rogue_meeting: Arc<ServiceTokenSigner>,
}

pub const MEETING_PERMISSIONS: &[&str] =
    &["messaging-application.delegate", "messaging-meeting.sync"];

fn peer_keys() -> &'static (ServiceSigningKey, ServiceSigningKey, ServiceSigningKey) {
    static KEYS: std::sync::OnceLock<(ServiceSigningKey, ServiceSigningKey, ServiceSigningKey)> =
        std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        (
            key("meeting-test-1"),
            key("privacy-relay-test-1"),
            key("sms-gateway-test-1"),
        )
    })
}

pub fn peers() -> Peers {
    let clock: SharedClock = Arc::new(SystemClock);
    let (m, p, s) = peer_keys();
    let signer = |id: ServiceId, k: &ServiceSigningKey| {
        let copy = ServiceSigningKey::from_pkcs8_pem(k.kid().clone(), &k.to_pkcs8_pem()).unwrap();
        Arc::new(ServiceTokenSigner::new(
            id,
            ENV,
            copy,
            TokenTtl::default(),
            clock.clone(),
        ))
    };
    Peers {
        meeting: signer(ServiceId::meeting_service(), m),
        privacy_relay: signer(ServiceId::privacy_relay(), p),
        sms_gateway: signer(ServiceId::sms_gateway(), s),
        rogue_meeting: signer(ServiceId::meeting_service(), &key("meeting-rogue-1")),
    }
}

/// Messaging's runtime: its trust bundle grants the Meeting Service, Privacy Relay and
/// the SMS Gateway exactly their Part V H permissions; Calling (a correctly signing peer)
/// is not trusted.
pub fn service_auth() -> (ServiceAuthRuntime, Arc<ServiceTokenSigner>) {
    let clock: SharedClock = Arc::new(SystemClock);
    let (m, p, s) = peer_keys();
    let issuer = |name: &str, k: &ServiceSigningKey, permissions: &[&str]| {
        json!({
            "issuer": name,
            "audience": cf_service_auth::services::MESSAGING,
            "runtime_environment": ENV,
            "keys": [k.public_jwk()],
            "permissions": permissions,
        })
    };
    let bundle = json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::MESSAGING,
        "runtime_environment": ENV,
        "trusted_issuers": [
            issuer(cf_service_auth::services::MEETING_SERVICE, m, MEETING_PERMISSIONS),
            issuer(cf_service_auth::services::PRIVACY_RELAY, p, &["messaging-context-authority.apply"]),
            issuer(cf_service_auth::services::SMS_GATEWAY, s, &["messaging-sms-bridge.deliver"]),
        ],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::messaging(), ENV).unwrap();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        ENV,
        key("messaging-test-1"),
        TokenTtl::default(),
        clock.clone(),
    ));
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock.clone()));
    let client = Arc::new(
        InternalServiceClient::new(
            signer.clone(),
            InternalTargets::new(ENV),
            std::time::Duration::from_secs(2),
        )
        .unwrap(),
    );
    let runtime = ServiceAuthRuntime::new(signer, verifier, client).unwrap();
    let untrusted = Arc::new(ServiceTokenSigner::new(
        ServiceId::calling(),
        ENV,
        key("calling-test-1"),
        TokenTtl::default(),
        clock,
    ));
    (runtime, untrusted)
}

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
    pub text: String,
    pub bytes: Vec<u8>,
}

impl Response {
    pub fn code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }
    pub fn reason(&self) -> &str {
        self.json["error"]["reason"].as_str().unwrap_or("")
    }
}

pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    pub authority: Arc<FakeAuthority>,
    pub principals: Arc<FakePrincipals>,
    pub untrusted: Arc<ServiceTokenSigner>,
    pub state: AppState,
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::build(|_| {}).await
    }

    pub async fn build(configure: impl FnOnce(&mut AppState)) -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        Some(Self::on(db, configure))
    }

    pub fn on(db: Arc<TestDb>, configure: impl FnOnce(&mut AppState)) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("messaging=warn")
            .try_init();
        let clock = ManualClock::new(t0());
        let authority = Arc::new(FakeAuthority::default());
        let principals = Arc::new(FakePrincipals::default());
        let (runtime, untrusted) = service_auth();
        let caching = Arc::new(CachingAuthority::new(
            authority.clone(),
            std::time::Duration::ZERO,
        ));
        let mut state = AppState::new(db.conn.clone(), Arc::new(clock.clone()), caching, runtime)
            .with_principals(principals.clone())
            .with_devices(DeviceSettings::default())
            .with_tree_head_signer(Arc::new(messaging::transparency::TreeHeadSigner::generate(
                "cfmeet-messaging:test",
            )))
            .with_managed_keys(Arc::new(
                messaging::messages::managed::ManagedKeyring::generate("test-1"),
            ));
        configure(&mut state);
        Self {
            db,
            clock,
            authority,
            principals,
            untrusted,
            state,
        }
    }

    pub fn router(&self) -> Router {
        http::app(self.state.clone())
    }

    pub fn conn(&self) -> &DatabaseConnection {
        &self.db.conn
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.state.now()
    }

    /// A new customer environment (tenant / application / environment) with a credential
    /// holding `scopes`.
    pub fn customer(&self, scopes: &[&str]) -> Customer {
        let context = TrustedContext {
            scope: OwnerScope {
                tenant_id: Uuid::new_v4(),
                application_id: Uuid::new_v4(),
                environment_id: Uuid::new_v4(),
                environment_kind: EnvironmentKind::Sandbox,
                service_account_id: Uuid::new_v4(),
            },
            credential_id: Uuid::new_v4(),
            first_party: false,
            full_authority: true,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            principal_restriction: None,
        };
        self.register_token(context)
    }

    /// Another credential of the same environment.
    pub fn credential(&self, of: &Customer, scopes: &[&str]) -> Customer {
        let mut context = of.context.clone();
        context.credential_id = Uuid::new_v4();
        context.scopes = scopes.iter().map(|s| s.to_string()).collect();
        self.register_token(context)
    }

    pub fn restricted(&self, of: &Customer, principals: &[Uuid]) -> Customer {
        let mut context = of.context.clone();
        context.credential_id = Uuid::new_v4();
        context.principal_restriction = Some(principals.iter().copied().collect::<BTreeSet<_>>());
        self.register_token(context)
    }

    pub fn register_token(&self, context: TrustedContext) -> Customer {
        let token = format!("cfmk_test_{}_{}", Uuid::new_v4().simple(), "a".repeat(64));
        self.authority
            .tokens
            .lock()
            .unwrap()
            .insert(token.clone(), context.clone());
        Customer { token, context }
    }

    /// A new ACTIVE principal of `customer`'s environment.
    pub fn principal(&self, customer: &Customer) -> Uuid {
        self.principal_of_kind(customer, "EXTERNAL_APPLICATION_PRINCIPAL")
    }

    pub fn principal_of_kind(&self, customer: &Customer, kind: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.set_principal(customer, id, PrincipalStatus::Active { kind: kind.into() });
        id
    }

    pub fn set_principal(&self, customer: &Customer, id: Uuid, status: PrincipalStatus) {
        self.principals
            .known
            .lock()
            .unwrap()
            .insert((customer.context.scope.environment_id, id), status);
    }

    pub async fn raw(&self, request: Request<Body>) -> Response {
        let response = self.router().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Response {
            status,
            headers,
            json,
            text,
            bytes: bytes.to_vec(),
        }
    }

    pub async fn send(
        &self,
        method: Method,
        token: Option<&str>,
        acting: Option<Uuid>,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Response {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if let Some(p) = acting {
            b = b.header("cf-acting-principal", p.to_string());
        }
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        self.raw(b.body(body).unwrap()).await
    }

    pub async fn get(&self, c: &Customer, acting: Option<Uuid>, path: &str) -> Response {
        self.send(Method::GET, Some(&c.token), acting, path, None, &[])
            .await
    }

    pub async fn post(
        &self,
        c: &Customer,
        acting: Option<Uuid>,
        path: &str,
        body: Value,
    ) -> Response {
        self.send(Method::POST, Some(&c.token), acting, path, Some(body), &[])
            .await
    }

    /// An internal request signed by `signer`.
    /// A signed internal call with an optional delegated scope.
    pub async fn call_internal(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        delegated: Option<cf_service_auth::DelegatedPlatformContext>,
        extra: &[(&str, &str)],
    ) -> Response {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::messaging());
        if let Some(d) = delegated {
            outbound = outbound.with_delegated_context(d);
        }
        let signed = signer.sign(&outbound).unwrap();
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        self.raw(b.body(body).unwrap()).await
    }

    /// A first-party application environment whose backend acts through a peer service
    /// (registered with the scripted Platform Access as a valid delegated scope).
    pub fn first_party(&self, first_party: bool) -> Customer {
        let c = self.customer(flows::ALL_SCOPES);
        let mut context = c.context.clone();
        context.first_party = first_party;
        self.authority
            .tokens
            .lock()
            .unwrap()
            .insert(c.token.clone(), context.clone());
        self.authority.delegated.lock().unwrap().insert(
            context.scope.environment_id,
            (context.scope, first_party, true),
        );
        Customer {
            token: c.token,
            context,
        }
    }

    pub async fn internal(&self, signer: &ServiceTokenSigner, path: &str) -> Response {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::messaging(),
            ))
            .unwrap();
        let mut b = Request::builder().method(Method::GET).uri(path);
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
        self.raw(b.body(Body::empty()).unwrap()).await
    }

    // -----------------------------------------------------------------------------------
    // Devices (real OpenMLS clients)
    // -----------------------------------------------------------------------------------

    pub async fn challenge(&self, c: &Customer, principal: Uuid) -> Response {
        self.post(
            c,
            Some(principal),
            "/v1/messaging/devices/registration-challenges",
            json!({}),
        )
        .await
    }

    /// Registers a new device of `principal`; returns the client device and the response.
    pub async fn register_device(&self, c: &Customer, principal: Uuid) -> (Device, Response) {
        let ch = self.challenge(c, principal).await;
        assert_eq!(ch.status, StatusCode::CREATED, "{}", ch.text);
        let device_id = Uuid::parse_str(ch.json["deviceId"].as_str().unwrap()).unwrap();
        let challenge_id = Uuid::parse_str(ch.json["challengeId"].as_str().unwrap()).unwrap();
        let nonce: [u8; 32] = hex::decode(ch.json["nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let device = Device::generate(device_id).unwrap();
        let kp = device
            .registration_key_package(challenge_id, &nonce)
            .unwrap();
        let r = self
            .post(
                c,
                Some(principal),
                "/v1/messaging/devices",
                json!({ "challengeId": challenge_id, "keyPackage": b64(&kp), "displayLabel": "test phone" }),
            )
            .await;
        (device, r)
    }

    pub async fn active_device(&self, c: &Customer, principal: Uuid) -> Device {
        let (device, r) = self.register_device(c, principal).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        device
    }

    pub async fn publish(
        &self,
        c: &Customer,
        principal: Uuid,
        device_id: Uuid,
        packages: &[Vec<u8>],
    ) -> Response {
        self.post(
            c,
            Some(principal),
            &format!("/v1/messaging/devices/{device_id}/key-packages"),
            json!({ "keyPackages": packages.iter().map(|p| b64(p)).collect::<Vec<_>>() }),
        )
        .await
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        self.conn()
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<i64>(0)
            .unwrap()
    }

    pub async fn sql(&self, sql: &str) -> Result<(), String> {
        self.conn()
            .execute_unprepared(sql)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Every row of every table as JSON text (canary scans).
    pub async fn dump(&self) -> String {
        let rows = self
            .conn()
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'".to_string(),
            ))
            .await
            .unwrap();
        let mut out = String::new();
        for r in rows {
            let t: String = r.try_get_by_index(0).unwrap();
            for row in self
                .conn()
                .query_all(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!("SELECT to_jsonb(x)::text FROM {t} x"),
                ))
                .await
                .unwrap()
            {
                out.push_str(&row.try_get_by_index::<String>(0).unwrap());
                out.push('\n');
            }
        }
        out
    }
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &Value) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text.as_str().unwrap()).unwrap()
}

pub fn uuid(v: &Value) -> Uuid {
    Uuid::parse_str(v.as_str().unwrap_or_else(|| panic!("not a uuid: {v}"))).unwrap()
}

/// Validates `value` against a definition of a checked-in contract
/// (e.g. `messaging/common/v1.schema.json#/$defs/Device`).
#[track_caller]
pub fn conforms(reference: &str, value: &Value) {
    use jsonschema::{Registry, Resource, Validator};
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
    };
    // Compiled once per schema reference and process (the contracts do not change while
    // tests run); every frame and response is still validated.
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Validator>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cached = cache.lock().unwrap().get(reference).cloned();
    let validator = match cached {
        Some(v) => v,
        None => {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/contracts");
            let read = |rel: &str| -> Value {
                serde_json::from_str(&std::fs::read_to_string(dir.join(rel)).unwrap()).unwrap()
            };
            let manifest = read("manifest.json");
            let mut builder = Registry::new();
            for c in manifest["contracts"].as_array().unwrap() {
                if c["kind"] == "json-schema" {
                    let doc = read(c["path"].as_str().unwrap());
                    let id = doc["$id"].as_str().unwrap().to_string();
                    builder = builder.add(id, Resource::from_contents(doc)).unwrap();
                }
            }
            let registry = builder.prepare().unwrap();
            let v = Arc::new(
                jsonschema::options()
                    .with_registry(&registry)
                    .should_validate_formats(true)
                    .build(&json!({
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "$ref": format!("https://schemas.cfmeet.com/{reference}")
                    }))
                    .unwrap(),
            );
            cache
                .lock()
                .unwrap()
                .insert(reference.to_string(), v.clone());
            v
        }
    };
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{reference} violated: {errors:?}\n{value:#}"
    );
}
