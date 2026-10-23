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
    /// Credentials that no longer authenticate.
    pub revoked: StdMutex<Vec<Uuid>>,
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

    async fn credential_allows(
        &self,
        _delegated: &DelegatedPlatformContext,
        _correlation: &CorrelationId,
        credential_id: Uuid,
        scopes: &[&str],
    ) -> Result<bool, AuthorityUnavailable> {
        match self.answer.lock().unwrap().clone() {
            None => Err(AuthorityUnavailable("down".into())),
            Some(_) => {
                Ok(scopes.is_empty() && !self.revoked.lock().unwrap().contains(&credential_id))
            }
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
    .with_initiating_credential(Uuid::new_v4())
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

// ---------------------------------------------------------------------------
// Section 50: an in-memory Platform Access (delegation + evaluation) and sessions.
// The real processes are exercised by the API Gateway's cross-service journeys.
// ---------------------------------------------------------------------------

use developer_platform::{
    platform_access::{Delegated, DelegationActor, DelegationCall},
    session::{HumanSessions, HumanUser, SessionAuthorityUnavailable, SessionOutcome},
    webhooks::{WebhookService, WebhookSettings},
};

#[derive(Debug, Clone)]
pub struct MemTenant {
    pub classification: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct MemApp {
    pub tenant: Uuid,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct MemEnv {
    pub tenant: Uuid,
    pub app: Uuid,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct MemServiceAccount {
    pub tenant: Uuid,
    pub app: Uuid,
    pub env: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Default)]
pub struct MemoryPlatformAccess {
    pub tenants: StdMutex<std::collections::HashMap<Uuid, MemTenant>>,
    pub apps: StdMutex<std::collections::HashMap<Uuid, MemApp>>,
    pub envs: StdMutex<std::collections::HashMap<Uuid, MemEnv>>,
    pub service_accounts: StdMutex<std::collections::HashMap<Uuid, MemServiceAccount>>,
    /// credential id -> scopes (machine callers).
    pub credential_scopes: StdMutex<std::collections::HashMap<Uuid, Vec<String>>>,
    /// Delegated mutations: (actor header, call).
    pub audit: StdMutex<Vec<(String, DelegationCall)>>,
    pub down: std::sync::atomic::AtomicBool,
}

impl MemoryPlatformAccess {
    pub fn first_party_tenant(&self) -> Uuid {
        let id = Uuid::new_v4();
        self.tenants.lock().unwrap().insert(
            id,
            MemTenant {
                classification: "FIRST_PARTY".into(),
                status: "ACTIVE".into(),
            },
        );
        id
    }

    pub fn tenant_status(&self, id: Uuid) -> String {
        self.tenants.lock().unwrap()[&id].status.clone()
    }

    pub fn app_status(&self, id: Uuid) -> String {
        self.apps.lock().unwrap()[&id].status.clone()
    }

    pub fn env_status(&self, id: Uuid) -> String {
        self.envs.lock().unwrap()[&id].status.clone()
    }

    pub fn sa_status(&self, id: Uuid) -> String {
        self.service_accounts.lock().unwrap()[&id].status.clone()
    }

    fn view_sa(id: Uuid, sa: &MemServiceAccount) -> Value {
        json!({
            "id": id, "tenantId": sa.tenant, "applicationId": sa.app, "environmentId": sa.env,
            "name": sa.name, "description": sa.description, "status": sa.status,
            "revokedAt": null, "revision": 1,
            "createdAt": sa.created_at.to_rfc3339(), "updatedAt": sa.created_at.to_rfc3339(),
        })
    }

    /// A machine credential of `env` holding `scopes`; returns its delegated context.
    /// Revokes the credential of a machine context (it stops authenticating).
    pub fn revoke(&self, machine: &DelegatedPlatformContext) {
        if let Some(c) = machine.initiating_credential_id() {
            self.credential_scopes.lock().unwrap().remove(&c);
        }
    }

    pub fn machine(&self, env: Uuid, scopes: &[&str]) -> DelegatedPlatformContext {
        let e = self.envs.lock().unwrap()[&env].clone();
        let credential = Uuid::new_v4();
        self.credential_scopes
            .lock()
            .unwrap()
            .insert(credential, scopes.iter().map(|s| s.to_string()).collect());
        let kind = match e.kind.as_str() {
            "TEST" => cf_service_auth::ApplicationEnvironmentKind::Test,
            "PRODUCTION" => cf_service_auth::ApplicationEnvironmentKind::Production,
            _ => cf_service_auth::ApplicationEnvironmentKind::Sandbox,
        };
        DelegatedPlatformContext::new(e.tenant, e.app, env, kind, Uuid::new_v4())
            .with_initiating_credential(credential)
    }

    fn view_tenant(id: Uuid, t: &MemTenant) -> Value {
        json!({ "id": id, "classification": t.classification, "status": t.status })
    }
}

#[async_trait]
impl PlatformAccessPort for MemoryPlatformAccess {
    async fn trusted_context(
        &self,
        delegated: &DelegatedPlatformContext,
        _correlation: &CorrelationId,
    ) -> Result<ContextOutcome, AuthorityUnavailable> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AuthorityUnavailable("down".into()));
        }
        let envs = self.envs.lock().unwrap();
        let tenants = self.tenants.lock().unwrap();
        match envs.get(&delegated.application_environment_id()) {
            Some(e) if e.tenant == delegated.tenant_id() && e.app == delegated.application_id() => {
                let suspended = tenants.get(&e.tenant).is_some_and(|t| t.status != "ACTIVE");
                Ok(ContextOutcome::Confirmed(CanonicalContext {
                    tenant_id: e.tenant,
                    application_id: e.app,
                    environment_id: delegated.application_environment_id(),
                    environment_kind: e.kind.clone(),
                    service_account_id: delegated.initiating_service_account_id(),
                    authority_mode: if suspended { "SAFETY_ONLY" } else { "FULL" }.into(),
                }))
            }
            _ => Ok(ContextOutcome::Refused {
                code: "DELEGATED_CONTEXT_INVALID".into(),
            }),
        }
    }

    async fn scope_names(&self) -> Result<Vec<String>, AuthorityUnavailable> {
        Ok(vec![])
    }

    async fn credential_allows(
        &self,
        _delegated: &DelegatedPlatformContext,
        _correlation: &CorrelationId,
        credential_id: Uuid,
        scopes: &[&str],
    ) -> Result<bool, AuthorityUnavailable> {
        let map = self.credential_scopes.lock().unwrap();
        // An unknown (or revoked) credential never authenticates.
        let Some(held) = map.get(&credential_id).cloned() else {
            return Ok(false);
        };
        Ok(scopes.iter().all(|s| held.iter().any(|h| h == s)))
    }

    async fn delegate(
        &self,
        call: DelegationCall,
        _correlation: &CorrelationId,
    ) -> Result<Delegated, AuthorityUnavailable> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AuthorityUnavailable("down".into()));
        }
        let (_, _, _, actor) = call.request();
        if let Some(a) = actor {
            self.audit
                .lock()
                .unwrap()
                .push((a.header_value(), call.clone()));
        }
        let refused = |status: u16, code: &str| {
            Ok(Delegated::Refused {
                status,
                code: code.into(),
            })
        };
        let status = |s: bool| if s { "SUSPENDED" } else { "ACTIVE" }.to_string();
        match call {
            DelegationCall::CreateTenant { .. } => {
                let id = Uuid::new_v4();
                let t = MemTenant {
                    classification: "EXTERNAL".into(),
                    status: "ACTIVE".into(),
                };
                let v = Self::view_tenant(id, &t);
                self.tenants.lock().unwrap().insert(id, t);
                Ok(Delegated::Ok(v))
            }
            DelegationCall::GetTenant { tenant_id } => {
                match self.tenants.lock().unwrap().get(&tenant_id) {
                    Some(t) => Ok(Delegated::Ok(Self::view_tenant(tenant_id, t))),
                    None => refused(404, "NOT_FOUND"),
                }
            }
            DelegationCall::SetTenantStatus {
                tenant_id,
                suspended,
                ..
            } => {
                let mut ts = self.tenants.lock().unwrap();
                let Some(t) = ts.get_mut(&tenant_id) else {
                    return refused(404, "NOT_FOUND");
                };
                t.status = status(suspended);
                Ok(Delegated::Ok(Self::view_tenant(tenant_id, t)))
            }
            DelegationCall::CreateApplication { tenant_id, .. } => {
                if !self.tenants.lock().unwrap().contains_key(&tenant_id) {
                    return refused(404, "NOT_FOUND");
                }
                let id = Uuid::new_v4();
                self.apps.lock().unwrap().insert(
                    id,
                    MemApp {
                        tenant: tenant_id,
                        status: "ACTIVE".into(),
                    },
                );
                Ok(Delegated::Ok(
                    json!({ "id": id, "tenantId": tenant_id, "status": "ACTIVE" }),
                ))
            }
            DelegationCall::GetApplication { application_id } => {
                match self.apps.lock().unwrap().get(&application_id) {
                    Some(a) => Ok(Delegated::Ok(
                        json!({ "id": application_id, "tenantId": a.tenant, "status": a.status }),
                    )),
                    None => refused(404, "NOT_FOUND"),
                }
            }
            DelegationCall::SetApplicationStatus {
                application_id,
                suspended,
                ..
            } => {
                let mut apps = self.apps.lock().unwrap();
                let Some(a) = apps.get_mut(&application_id) else {
                    return refused(404, "NOT_FOUND");
                };
                a.status = status(suspended);
                Ok(Delegated::Ok(
                    json!({ "id": application_id, "status": a.status }),
                ))
            }
            DelegationCall::CreateEnvironment {
                application_id,
                kind,
                ..
            } => {
                if kind == "PRODUCTION" {
                    return refused(422, "POLICY_VIOLATION");
                }
                let Some(app) = self.apps.lock().unwrap().get(&application_id).cloned() else {
                    return refused(404, "NOT_FOUND");
                };
                let id = Uuid::new_v4();
                self.envs.lock().unwrap().insert(
                    id,
                    MemEnv {
                        tenant: app.tenant,
                        app: application_id,
                        kind: kind.clone(),
                        status: "ACTIVE".into(),
                    },
                );
                Ok(Delegated::Ok(
                    json!({ "id": id, "tenantId": app.tenant, "applicationId": application_id, "kind": kind, "status": "ACTIVE", "liveTelecomAllowed": false }),
                ))
            }
            DelegationCall::GetEnvironment { environment_id } => {
                match self.envs.lock().unwrap().get(&environment_id) {
                    Some(e) => Ok(Delegated::Ok(
                        json!({ "id": environment_id, "kind": e.kind, "status": e.status, "liveTelecomAllowed": false }),
                    )),
                    None => refused(404, "NOT_FOUND"),
                }
            }
            DelegationCall::SetEnvironmentStatus {
                environment_id,
                suspended,
                ..
            } => {
                let mut envs = self.envs.lock().unwrap();
                let Some(e) = envs.get_mut(&environment_id) else {
                    return refused(404, "NOT_FOUND");
                };
                e.status = status(suspended);
                Ok(Delegated::Ok(
                    json!({ "id": environment_id, "status": e.status }),
                ))
            }
            DelegationCall::ListApplicationScopes { .. } => {
                Ok(Delegated::Ok(json!({ "scopes": [] })))
            }
            // Platform Access's rules: the environment's own tenant / application, unique
            // names, PRODUCTION refused, scoped to the environment, never revoked here.
            DelegationCall::CreateServiceAccount {
                environment_id,
                name,
                description,
                ..
            } => {
                let Some(env) = self.envs.lock().unwrap().get(&environment_id).cloned() else {
                    return refused(404, "NOT_FOUND");
                };
                if env.kind == "PRODUCTION" {
                    return refused(422, "POLICY_VIOLATION");
                }
                let mut sas = self.service_accounts.lock().unwrap();
                if sas
                    .values()
                    .any(|s| s.env == environment_id && s.name == name)
                {
                    return refused(409, "CONFLICT");
                }
                let id = Uuid::new_v4();
                let sa = MemServiceAccount {
                    tenant: env.tenant,
                    app: env.app,
                    env: environment_id,
                    name,
                    description,
                    status: "ACTIVE".into(),
                    created_at: chrono::Utc::now(),
                };
                let v = Self::view_sa(id, &sa);
                sas.insert(id, sa);
                Ok(Delegated::Ok(v))
            }
            DelegationCall::ListServiceAccounts { environment_id } => {
                if !self.envs.lock().unwrap().contains_key(&environment_id) {
                    return refused(404, "NOT_FOUND");
                }
                let sas = self.service_accounts.lock().unwrap();
                let list: Vec<Value> = sas
                    .iter()
                    .filter(|(_, s)| s.env == environment_id)
                    .map(|(id, s)| Self::view_sa(*id, s))
                    .collect();
                Ok(Delegated::Ok(json!({ "serviceAccounts": list })))
            }
            DelegationCall::GetServiceAccount {
                environment_id,
                service_account_id,
            } => match self
                .service_accounts
                .lock()
                .unwrap()
                .get(&service_account_id)
            {
                Some(s) if s.env == environment_id => {
                    Ok(Delegated::Ok(Self::view_sa(service_account_id, s)))
                }
                _ => refused(404, "NOT_FOUND"),
            },
            DelegationCall::SetServiceAccountStatus {
                environment_id,
                service_account_id,
                suspended,
                ..
            } => {
                let kind = self
                    .envs
                    .lock()
                    .unwrap()
                    .get(&environment_id)
                    .map(|e| e.kind.clone());
                let mut sas = self.service_accounts.lock().unwrap();
                let Some(s) = sas
                    .get_mut(&service_account_id)
                    .filter(|s| s.env == environment_id)
                else {
                    return refused(404, "NOT_FOUND");
                };
                if kind.as_deref() == Some("PRODUCTION") {
                    return refused(422, "POLICY_VIOLATION");
                }
                s.status = status(suspended);
                Ok(Delegated::Ok(Self::view_sa(service_account_id, s)))
            }
        }
    }
}

/// Session tokens -> verified humans.
#[derive(Default)]
pub struct MemorySessions {
    pub users: StdMutex<std::collections::HashMap<String, HumanUser>>,
    pub down: std::sync::atomic::AtomicBool,
}

impl MemorySessions {
    /// A signed-in human with a verified email; returns the session token.
    pub fn sign_in(&self, email: &str) -> (String, Uuid) {
        let token = format!("session-{}", Uuid::new_v4().simple());
        let user = Uuid::new_v4();
        self.users.lock().unwrap().insert(
            token.clone(),
            HumanUser {
                user_id: user,
                verified_email: Some(email.to_string()),
            },
        );
        (token, user)
    }
}

#[async_trait]
impl HumanSessions for MemorySessions {
    async fn verify(
        &self,
        session_token: &str,
        _correlation: &CorrelationId,
    ) -> Result<SessionOutcome, SessionAuthorityUnavailable> {
        if self.down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SessionAuthorityUnavailable("down".into()));
        }
        Ok(match self.users.lock().unwrap().get(session_token) {
            Some(u) => SessionOutcome::Verified(u.clone()),
            None => SessionOutcome::Rejected,
        })
    }
}

/// The Section 50 harness: real router, real gateway tokens, in-memory authorities.
pub struct Control {
    pub db: TestDb,
    pub router: Router,
    pub peers: Peers,
    pub pa: Arc<MemoryPlatformAccess>,
    pub sessions: Arc<MemorySessions>,
    pub operator: Arc<ServiceTokenSigner>,
}

pub enum As<'a> {
    User(&'a str),
    Machine(&'a DelegatedPlatformContext),
    Nobody,
}

/// Section 53 webhook fixtures of a [`Control`] harness.
pub struct Webhooks {
    /// The service the router uses (same settings, clock and database).
    pub service: WebhookService,
    pub producers: Producers,
    /// Seconds added to the wall clock (tests advance time).
    pub offset: Arc<std::sync::atomic::AtomicI64>,
}

impl Webhooks {
    pub fn advance(&self, seconds: i64) {
        self.offset
            .fetch_add(seconds, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Control {
    pub async fn new() -> Option<Self> {
        Self::build(None).await.map(|(c, _)| c)
    }

    /// A harness whose router serves central webhooks with `settings` (enabled).
    pub async fn with_webhooks(mut settings: WebhookSettings) -> Option<(Self, Webhooks)> {
        settings.enabled = true;
        Self::build(Some(settings))
            .await
            .map(|(c, w)| (c, w.expect("webhooks configured")))
    }

    /// [`Control::with_webhooks`] whose state is further configured by `configure` (e.g.
    /// the Section 53 migration's Calling port).
    pub async fn with_webhooks_configured(
        mut settings: WebhookSettings,
        configure: impl FnOnce(AppState) -> AppState,
    ) -> Option<(Self, Webhooks)> {
        settings.enabled = true;
        Self::build_with(Some(settings), configure)
            .await
            .map(|(c, w)| (c, w.expect("webhooks configured")))
    }

    /// A harness whose webhook service is configured but NOT enabled (the flag off).
    pub async fn with_webhooks_disabled(mut settings: WebhookSettings) -> Option<(Self, Webhooks)> {
        settings.enabled = false;
        Self::build(Some(settings))
            .await
            .map(|(c, w)| (c, w.expect("webhooks configured")))
    }

    async fn build(webhooks: Option<WebhookSettings>) -> Option<(Self, Option<Webhooks>)> {
        Self::build_with(webhooks, |s| s).await
    }

    async fn build_with(
        webhooks: Option<WebhookSettings>,
        configure: impl FnOnce(AppState) -> AppState,
    ) -> Option<(Self, Option<Webhooks>)> {
        let db = TestDb::new().await?;
        let (runtime, peers, operator, producers) = service_auth_with_producers();
        let pa = Arc::new(MemoryPlatformAccess::default());
        let sessions = Arc::new(MemorySessions::default());
        let mut state = AppState::new(db.conn.clone(), runtime)
            .with_platform_access(pa.clone())
            .with_sessions(sessions.clone())
            .with_flags(FeatureFlags { gateway_api: true });
        let webhooks = webhooks.map(|settings| {
            let offset = Arc::new(std::sync::atomic::AtomicI64::new(0));
            let o = offset.clone();
            let service =
                WebhookService::new(db.conn.clone(), settings).with_clock(Arc::new(move || {
                    chrono::Utc::now()
                        + chrono::Duration::seconds(o.load(std::sync::atomic::Ordering::SeqCst))
                }));
            state = state.clone().with_webhooks(service.clone());
            Webhooks {
                service,
                producers,
                offset,
            }
        });
        let state = configure(state);
        Some((
            Self {
                router: http::app(state),
                db,
                peers,
                pa,
                sessions,
                operator,
            },
            webhooks,
        ))
    }

    /// A service-to-service request signed by `signer` (internal routes).
    pub async fn internal(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::developer_platform(),
            ))
            .unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        self.send(method, path, headers, body).await
    }

    async fn send(
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
            Some(b) => {
                request = request.header("content-type", "application/json");
                Body::from(b.to_string())
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

    /// A request with exactly `headers`.
    pub async fn raw(
        &self,
        method: Method,
        path: String,
        headers: HeaderMap,
        body: Option<Value>,
    ) -> Response {
        self.send(method, &path, headers, body).await
    }

    /// A gateway-forwarded customer request.
    pub async fn req(
        &self,
        who: As<'_>,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        self.req_with(who, method, path, body, &[]).await
    }

    pub async fn req_with(
        &self,
        who: As<'_>,
        method: Method,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Response {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        if let As::Machine(d) = &who {
            outbound = outbound.with_delegated_context((*d).clone());
        }
        let signed = self.peers.gateway.sign(&outbound).unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        if let As::User(token) = who {
            headers.insert("x-cf-user-session", token.parse().unwrap());
        }
        for (k, v) in extra {
            headers.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        self.send(method, path, headers, body).await
    }

    /// An operator-console command.
    pub async fn operator(&self, method: Method, path: &str, body: Option<Value>) -> Response {
        let signed = self
            .operator
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::developer_platform(),
            ))
            .unwrap();
        let mut headers = HeaderMap::new();
        signed.apply_to(&mut headers);
        headers.insert("x-cf-operator", "ops.tester".parse().unwrap());
        self.send(method, path, headers, body).await
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

/// Producers of platform events (Section 53), each granted `developer-event.publish`.
pub struct Producers {
    pub calling: Arc<ServiceTokenSigner>,
    pub messaging: Arc<ServiceTokenSigner>,
    pub numbering: Arc<ServiceTokenSigner>,
    pub meeting: Arc<ServiceTokenSigner>,
    pub privacy_relay: Arc<ServiceTokenSigner>,
    pub sms: Arc<ServiceTokenSigner>,
}

/// [`service_auth_with_operator`] plus the event producers.
pub fn service_auth_with_producers() -> (
    ServiceAuthRuntime,
    Peers,
    Arc<ServiceTokenSigner>,
    Producers,
) {
    use cf_service_auth::services as svc;
    let clock: SharedClock = Arc::new(SystemClock);
    let me = key("developer-platform-test-1");
    let gateway = key("api-gateway-test-1");
    let messaging = key("messaging-test-1");
    let operator = key("operator-test-1");
    let producer_keys: Vec<(&str, ServiceSigningKey)> = [
        svc::CALLING,
        svc::MESSAGING,
        svc::NUMBERING,
        svc::MEETING_SERVICE,
        svc::PRIVACY_RELAY,
        svc::SMS_GATEWAY,
    ]
    .into_iter()
    .map(|s| {
        (
            s,
            key(&format!("{}-producer-1", s.trim_start_matches("cfmeet."))),
        )
    })
    .collect();
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({
        "issuer": id, "audience": svc::DEVELOPER_PLATFORM, "runtime_environment": ENV,
        "keys": [k.public_jwk()], "permissions": perms })
    };
    let mut issuers = vec![
        issuer(
            svc::API_GATEWAY,
            &gateway,
            &[
                "developer-request.forward",
                "developer-entitlement.read",
                "developer-usage.record",
            ],
        ),
        issuer(
            svc::OPERATOR_CONSOLE,
            &operator,
            &["developer-platform.administer"],
        ),
    ];
    for (s, k) in &producer_keys {
        issuers.push(issuer(s, k, &["developer-event.publish"]));
    }
    let bundle = json!({
        "bundle_version": 1, "receiver": svc::DEVELOPER_PLATFORM, "runtime_environment": ENV,
        "trusted_issuers": issuers,
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
    let mut keys = producer_keys.into_iter();
    let mut next = || {
        let (s, k) = keys.next().unwrap();
        signer(ServiceId::parse(s).unwrap(), k)
    };
    let producers = Producers {
        calling: next(),
        messaging: next(),
        numbering: next(),
        meeting: next(),
        privacy_relay: next(),
        sms: next(),
    };
    (
        runtime,
        Peers {
            gateway: signer(ServiceId::api_gateway(), gateway),
            messaging: signer(ServiceId::messaging(), messaging),
            rogue_gateway: signer(ServiceId::api_gateway(), key("api-gateway-test-1")),
        },
        signer(ServiceId::operator_console(), operator),
        producers,
    )
}

/// [`service_auth`] plus an operator-console signer granted `developer-platform.administer`
/// and the gateway's entitlement permission.
pub fn service_auth_with_operator() -> (ServiceAuthRuntime, Peers, Arc<ServiceTokenSigner>) {
    let clock: SharedClock = Arc::new(SystemClock);
    let me = key("developer-platform-test-1");
    let gateway = key("api-gateway-test-1");
    let messaging = key("messaging-test-1");
    let operator = key("operator-test-1");
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({
        "issuer": id, "audience": cf_service_auth::services::DEVELOPER_PLATFORM, "runtime_environment": ENV,
        "keys": [k.public_jwk()], "permissions": perms })
    };
    let bundle = json!({
        "bundle_version": 1, "receiver": cf_service_auth::services::DEVELOPER_PLATFORM, "runtime_environment": ENV,
        "trusted_issuers": [
            issuer(cf_service_auth::services::API_GATEWAY, &gateway, &["developer-request.forward", "developer-entitlement.read", "developer-usage.record"]),
            issuer(cf_service_auth::services::OPERATOR_CONSOLE, &operator, &["developer-platform.administer"]),
        ],
    }).to_string();
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
        signer(ServiceId::operator_console(), operator),
    )
}

#[allow(unused)]
fn _actor_kinds(a: DelegationActor) -> String {
    a.header_value()
}
