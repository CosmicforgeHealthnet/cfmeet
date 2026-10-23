//! Shared harness for Developer Platform integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets
//! its own database cloned from a freshly migrated template. Without it the tests FAIL,
//! unless `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED line).
//!
//! Requests go through the REAL router with REAL Ed25519 service tokens verified by the
//! Developer Platform's trust bundle; Platform Access is a scripted port here (the real
//! process is exercised by the API Gateway's cross-service journey).
#![allow(dead_code)]

use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    CorrelationId, DelegatedPlatformContext, KeyId, OutboundServiceRequest, RuntimeEnvironment,
    ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier, SharedClock,
    SystemClock, TokenTtl, TrustPolicy,
};
use developer_platform::{
    config::{service_auth::ServiceAuthRuntime, FeatureFlags},
    http,
    platform_access::{AuthorityUnavailable, CanonicalContext, ContextOutcome, PlatformAccessPort},
    state::AppState,
};
use developer_platform_migration::MigratorTrait;
use http_body_util::BodyExt;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

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
                    "TEST_DATABASE_URL is not set. Developer Platform integration tests require PostgreSQL \
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
            "SELECT datname FROM pg_database WHERE datname LIKE 'dp\\_t\\_%' OR datname LIKE 'dp\\_tmpl\\_%'",
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
            .trim_start_matches("dp_tmpl_")
            .trim_start_matches("dp_t_")
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
            let name = format!("dp_tmpl_{}_{}", std::process::id(), Uuid::new_v4().simple());
            let conn = Database::connect(admin).await.expect("connect admin db");
            drop_databases_of_dead_runs(&conn).await;
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            developer_platform_migration::Migrator::up(&tmpl, None)
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
        let name = format!("dp_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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

/// Peers of the Developer Platform in tests.
pub struct Peers {
    /// The API Gateway (granted `developer-request.forward`).
    pub gateway: Arc<ServiceTokenSigner>,
    /// Messaging: a real service, but not an issuer the Developer Platform trusts (the
    /// registry makes `developer-request.forward` grantable to the API Gateway only).
    pub messaging: Arc<ServiceTokenSigner>,
    /// A key nobody trusts, claiming to be the gateway.
    pub rogue_gateway: Arc<ServiceTokenSigner>,
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

/// The Developer Platform's runtime and its peers' signers.
pub fn service_auth() -> (ServiceAuthRuntime, Peers) {
    let clock: SharedClock = Arc::new(SystemClock);
    let me = key("developer-platform-test-1");
    let gateway = key("api-gateway-test-1");
    let messaging = key("messaging-test-1");
    let bundle = json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::DEVELOPER_PLATFORM,
        "runtime_environment": ENV,
        "trusted_issuers": [
            {
                "issuer": cf_service_auth::services::API_GATEWAY,
                "audience": cf_service_auth::services::DEVELOPER_PLATFORM,
                "runtime_environment": ENV,
                "keys": [gateway.public_jwk()],
                "permissions": ["developer-request.forward"],
            },
        ],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::developer_platform(), ENV).unwrap();
    let own = signer(ServiceId::developer_platform(), me);
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock));
    let client = Arc::new(
        InternalServiceClient::new(
            own.clone(),
            InternalTargets::new(ENV),
            std::time::Duration::from_secs(2),
        )
        .unwrap(),
    );
    let runtime = ServiceAuthRuntime::new(own, verifier, client).unwrap();
    (
        runtime,
        Peers {
            gateway: signer(ServiceId::api_gateway(), gateway),
            messaging: signer(ServiceId::messaging(), messaging),
            rogue_gateway: signer(ServiceId::api_gateway(), key("api-gateway-test-1")),
        },
    )
}

/// A scripted Platform Access.
#[derive(Default)]
pub struct FakePlatformAccess {
    /// None: the authority is down.
    pub answer: StdMutex<Option<Answer>>,
    pub calls: StdMutex<Vec<DelegatedPlatformContext>>,
}

#[derive(Clone)]
pub enum Answer {
    /// Confirm exactly the delegated chain.
    Echo,
    /// Confirm a different chain (a contradicting authority answer).
    Other,
    Refuse,
}

#[async_trait]
impl PlatformAccessPort for FakePlatformAccess {
    async fn trusted_context(
        &self,
        delegated: &DelegatedPlatformContext,
        _correlation: &CorrelationId,
    ) -> Result<ContextOutcome, AuthorityUnavailable> {
        self.calls.lock().unwrap().push(delegated.clone());
        let answer = self.answer.lock().unwrap().clone();
        let confirmed = |tenant: Uuid| CanonicalContext {
            tenant_id: tenant,
            application_id: delegated.application_id(),
            environment_id: delegated.application_environment_id(),
            environment_kind: "SANDBOX".into(),
            service_account_id: delegated.initiating_service_account_id(),
            authority_mode: "FULL".into(),
        };
        match answer {
            None => Err(AuthorityUnavailable("down".into())),
            Some(Answer::Echo) => Ok(ContextOutcome::Confirmed(confirmed(delegated.tenant_id()))),
            Some(Answer::Other) => Ok(ContextOutcome::Confirmed(confirmed(Uuid::new_v4()))),
            Some(Answer::Refuse) => Ok(ContextOutcome::Refused {
                code: "DELEGATED_CONTEXT_INVALID".into(),
            }),
        }
    }

    async fn scope_names(&self) -> Result<Vec<String>, AuthorityUnavailable> {
        Ok(vec![])
    }
}

pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
}

pub struct Harness {
    pub db: TestDb,
    pub router: Router,
    pub peers: Peers,
    pub pa: Arc<FakePlatformAccess>,
}

pub fn delegated() -> DelegatedPlatformContext {
    DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        cf_service_auth::ApplicationEnvironmentKind::Sandbox,
        Uuid::new_v4(),
    )
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with_flags(FeatureFlags { gateway_api: true }).await
    }

    pub async fn with_flags(flags: FeatureFlags) -> Option<Self> {
        let db = TestDb::new().await?;
        let (runtime, peers) = service_auth();
        let pa = Arc::new(FakePlatformAccess::default());
        *pa.answer.lock().unwrap() = Some(Answer::Echo);
        let state = AppState::new(db.conn.clone(), runtime)
            .with_platform_access(pa.clone())
            .with_flags(flags);
        Some(Self {
            router: http::app(state),
            db,
            peers,
            pa,
        })
    }

    pub async fn call(&self, method: Method, path: &str, headers: HeaderMap) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        for (k, v) in headers.iter() {
            request = request.header(k, v);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
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

    pub async fn get(&self, path: &str) -> Response {
        self.call(Method::GET, path, HeaderMap::new()).await
    }

    /// GET signed by `signer` (optionally with a delegated context).
    pub async fn signed_get(
        &self,
        signer: &ServiceTokenSigner,
        path: &str,
        delegated: Option<DelegatedPlatformContext>,
    ) -> Response {
        let mut request = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        if let Some(d) = delegated {
            request = request.with_delegated_context(d);
        }
        let signed = signer.sign(&request).unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        self.call(Method::GET, path, headers).await
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
}

/// Validates `value` against a definition of a checked-in contract
/// (e.g. `sms-gateway/common/v1.schema.json#/$defs/Error`).
#[track_caller]
pub fn conforms(reference: &str, value: &Value) {
    use jsonschema::{Registry, Resource};
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
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
    let validator = jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$ref": format!("https://schemas.cfmeet.com/{reference}")
        }))
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{reference} violated: {errors:?}\n{value:#}"
    );
}
