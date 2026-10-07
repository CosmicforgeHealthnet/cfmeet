//! Shared harness for Calling integration tests.
//!
//! Requires PostgreSQL (no Docker needed): set `TEST_DATABASE_URL` to an admin URL, e.g.
//! `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Each test gets its own
//! database cloned from a freshly migrated template and dropped afterwards. Without
//! `TEST_DATABASE_URL` the tests FAIL, unless `CFMEET_SKIP_DB_TESTS=1` is set — then they
//! print an explicit SKIPPED line (never a silent pass).
//!
//! The harness uses a deterministic, test-only pepper and a manually driven clock.
#![allow(dead_code)]

use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    routing::{get, post},
    Json, Router,
};
use calling::{
    config::CredentialPolicy,
    domain::{
        EnvironmentKind, ManualClock, PrincipalId, PrincipalKind, RequestId, ServiceAccountId,
        TenantClassification,
    },
    error::CallingError,
    platform::{
        applications::{ApplicationView, NewApplication},
        authentication::AuthenticationError,
        context::PlatformAuthContext,
        credentials::{IssueCredential, IssuedCredential},
        environments::EnvironmentView,
        limits::LimitDefaults,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::{names, ScopeRegistry},
        service_accounts::{NewServiceAccount, ServiceAccountView},
        tenants::{NewTenant, TenantView},
        verifier::{CredentialPepper, VerifierKeyring},
        PlatformService,
    },
    routes,
    state::AppState,
};
use calling_migration::MigratorTrait;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

/// Deterministic test-only pepper (never used outside tests).
pub const TEST_PEPPER: &str = "calling-integration-test-pepper-6c1f0e9b2d8a4f7e93b5d02a";

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
                    "TEST_DATABASE_URL is not set. Calling integration tests require PostgreSQL \
                     (e.g. TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres). \
                     Set CFMEET_SKIP_DB_TESTS=1 to skip explicitly."
                )
            }
        }
    }
}

fn with_db(admin: &str, db: &str) -> String {
    let (base, _) = admin
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL must include a database name");
    format!("{base}/{db}")
}

/// Drops databases of test processes that no longer exist (`clg_t_<pid>_…`).
async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    if !std::path::Path::new("/proc/self").exists() {
        return;
    }
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'clg\\_t\\_%' OR datname LIKE 'clg\\_tmpl\\_%'",
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
            .trim_start_matches("clg_tmpl_")
            .trim_start_matches("clg_t_")
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
                "clg_tmpl_{}_{}",
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
            calling_migration::Migrator::up(&tmpl, None)
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
    /// Migrated database.
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        let name = format!("clg_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = CREATE_LOCK.lock().await;
            let conn = Database::connect(&admin).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        Self::connect(admin, name).await
    }

    /// Empty database (no migrations), for migration tests.
    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        let name = format!("clg_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        let conn = Database::connect(&admin).await.unwrap();
        conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        conn.close().await.ok();
        Self::connect(admin, name).await
    }

    async fn connect(admin: String, name: String) -> Option<Self> {
        let url = with_db(&admin, &name);
        let mut opts = sea_orm::ConnectOptions::new(url.clone());
        opts.max_connections(20).sqlx_logging(false);
        let conn = Database::connect(opts).await.unwrap();
        Some(Self {
            conn,
            url,
            name,
            admin,
        })
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

/// Fixed start of the manual clock: mid-minute, so rate windows are deterministic.
pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2031-03-01T10:00:30Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub fn test_keyring(pepper: &str) -> VerifierKeyring {
    VerifierKeyring::new(
        1,
        CredentialPepper::parse("API_CREDENTIAL_PEPPER", pepper).unwrap(),
        None,
    )
    .unwrap()
}

/// Database + platform service + HTTP state wired with a manual clock.
pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    pub platform: PlatformService,
    pub state: AppState,
}

/// One consumer: tenant + application + all three environments, a `backend` service
/// account in each, and the standard scopes allowed.
pub struct Consumer {
    pub tenant: TenantView,
    pub application: ApplicationView,
    pub environments: HashMap<EnvironmentKind, EnvironmentView>,
    pub service_accounts: HashMap<EnvironmentKind, ServiceAccountView>,
}

impl Consumer {
    pub fn env(&self, kind: EnvironmentKind) -> &EnvironmentView {
        &self.environments[&kind]
    }

    pub fn sa(&self, kind: EnvironmentKind) -> ServiceAccountId {
        self.service_accounts[&kind].id
    }
}

pub const STANDARD_TEST_SCOPES: &[&str] = &[
    names::PRINCIPALS_READ,
    names::PRINCIPALS_WRITE,
    names::CALLS_READ,
    names::CALLS_WRITE,
];

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with_pepper(TEST_PEPPER).await
    }

    pub async fn with_pepper(pepper: &str) -> Option<Self> {
        let db = TestDb::new().await?;
        Some(Self::on(Arc::new(db), pepper, ManualClock::new(t0())))
    }

    /// Another deployment over the same database (e.g. a different pepper).
    pub fn sibling(&self, pepper: &str) -> Self {
        Self::on(self.db.clone(), pepper, self.clock.clone())
    }

    pub fn on(db: Arc<TestDb>, pepper: &str, clock: ManualClock) -> Self {
        let keyring = Arc::new(test_keyring(pepper));
        let registry = Arc::new(ScopeRegistry::standard());
        let platform = PlatformService::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            keyring.clone(),
            registry.clone(),
            CredentialPolicy::default(),
            LimitDefaults::default(),
        );
        let state = AppState::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            keyring,
            registry,
            LimitDefaults::default(),
        );
        Self {
            db,
            clock,
            platform,
            state,
        }
    }

    pub fn conn(&self) -> &DatabaseConnection {
        &self.db.conn
    }

    pub async fn consumer(&self, slug: &str, classification: TenantClassification) -> Consumer {
        let tenant = self
            .platform
            .create_tenant(NewTenant {
                slug: slug.to_string(),
                display_name: format!("{slug} tenant"),
                classification,
                billing_currency: None,
            })
            .await
            .unwrap();
        self.application_in(&tenant, "app").await
    }

    /// Another application (with environments, service accounts, standard scopes).
    pub async fn application_in(&self, tenant: &TenantView, slug: &str) -> Consumer {
        let application = self
            .platform
            .create_application(
                tenant.id,
                NewApplication {
                    slug: slug.to_string(),
                    display_name: format!("{slug} application"),
                },
            )
            .await
            .unwrap();
        for scope in STANDARD_TEST_SCOPES {
            self.platform
                .grant_application_scope(application.id, scope)
                .await
                .unwrap();
        }
        let mut environments = HashMap::new();
        let mut service_accounts = HashMap::new();
        for kind in EnvironmentKind::ALL {
            let env = self
                .platform
                .create_environment(application.id, *kind)
                .await
                .unwrap();
            let sa = self
                .platform
                .create_service_account(
                    env.id,
                    NewServiceAccount {
                        name: "backend".to_string(),
                        description: Some("test backend".to_string()),
                    },
                )
                .await
                .unwrap();
            environments.insert(*kind, env);
            service_accounts.insert(*kind, sa);
        }
        Consumer {
            tenant: tenant.clone(),
            application,
            environments,
            service_accounts,
        }
    }

    pub async fn issue(&self, sa: ServiceAccountId, scopes: &[&str]) -> IssuedCredential {
        self.platform
            .issue_credential(
                sa,
                IssueCredential {
                    scopes: scopes.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    }

    pub async fn authenticate(
        &self,
        token: &str,
    ) -> Result<PlatformAuthContext, AuthenticationError> {
        self.state
            .authenticator
            .authenticate(token, RequestId::generate())
            .await
    }

    pub async fn ctx(&self, token: &str) -> PlatformAuthContext {
        self.authenticate(token).await.expect("authenticates")
    }

    pub fn router(&self) -> Router {
        routes::app(self.state.clone(), test_routes())
    }

    /// One request through the full router (request id + authentication middleware).
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> TestResponse {
        self.request_with(method, path, token, body, &[]).await
    }

    pub async fn request_with(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        TestResponse {
            status,
            headers,
            text,
            json,
        }
    }

    /// GET /test/context with `token`.
    pub async fn whoami(&self, token: &str) -> TestResponse {
        self.request(Method::POST, "/test/context", Some(token), Some(json!({})))
            .await
    }
}

pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub text: String,
    pub json: Value,
}

impl TestResponse {
    /// Error body with the per-request id blanked, for byte-for-byte comparison.
    pub fn normalized_error(&self) -> Value {
        let mut v = self.json.clone();
        if let Some(error) = v.get_mut("error") {
            error["requestId"] = Value::Null;
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Test-only protected routes (never part of the service router).
// ---------------------------------------------------------------------------

pub fn context_json(ctx: &PlatformAuthContext) -> Value {
    json!({
        "tenant_id": ctx.tenant_id(),
        "tenant_classification": ctx.tenant_classification(),
        "application_id": ctx.application_id(),
        "environment_id": ctx.environment_id(),
        "environment_kind": ctx.environment_kind(),
        "live_telecom_allowed": ctx.live_telecom_allowed(),
        "service_account_id": ctx.service_account_id(),
        "credential_id": ctx.credential_id(),
        "scopes": ctx.scopes().names(),
        "request_id": ctx.request_id().as_str(),
    })
}

/// Echoes the authenticated context. The request body is accepted and ignored: it can
/// never influence the context.
async fn context_handler(ctx: PlatformAuthContext, _body: Option<Json<Value>>) -> Json<Value> {
    Json(context_json(&ctx))
}

async fn principals_read(ctx: PlatformAuthContext) -> Result<Json<Value>, CallingError> {
    ctx.require_scope(names::PRINCIPALS_READ)?;
    Ok(Json(json!({ "ok": true })))
}

async fn calls_write(ctx: PlatformAuthContext) -> Result<Json<Value>, CallingError> {
    ctx.require_scope(names::CALLS_WRITE)?;
    Ok(Json(json!({ "ok": true })))
}

async fn resolve_principal(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
    Json(body): Json<Value>,
) -> Result<Json<Value>, CallingError> {
    ctx.require_scope(names::PRINCIPALS_READ)?;
    let reference = match body.get("id").and_then(Value::as_str) {
        Some(id) => PrincipalReference::Id(PrincipalId::from_uuid(
            Uuid::parse_str(id).map_err(|_| CallingError::NotFound)?,
        )),
        None => PrincipalReference::External {
            kind: body
                .get("kind")
                .and_then(Value::as_str)
                .and_then(PrincipalKind::parse)
                .ok_or(CallingError::Validation("The principal kind is invalid."))?,
            external_reference: body
                .get("reference")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
    };
    match state.principals.resolve(&ctx, &reference).await {
        Ok(p) => Ok(Json(json!({
            "id": p.id(),
            "kind": p.kind(),
            "external_reference": p.external_reference(),
            "linked_service_account_id": p.linked_service_account_id(),
            "tenant_id": p.tenant_id(),
            "application_id": p.application_id(),
            "environment_id": p.environment_id(),
        }))),
        Err(PrincipalResolutionError::NotFound)
        | Err(PrincipalResolutionError::InvalidReference) => Err(CallingError::NotFound),
        Err(PrincipalResolutionError::Inactive) => {
            Err(CallingError::Validation("The principal is not active."))
        }
        Err(PrincipalResolutionError::Unavailable { .. }) => Err(CallingError::ServiceUnavailable),
    }
}

async fn limits_handler(
    State(state): State<AppState>,
    ctx: PlatformAuthContext,
) -> Result<Json<Value>, CallingError> {
    let limits = state.limits.effective_for(&ctx).await?;
    Ok(Json(serde_json::to_value(limits).unwrap()))
}

pub fn test_routes() -> Router<AppState> {
    Router::new()
        .route("/test/context", post(context_handler).get(context_handler))
        .route("/test/principals-read", get(principals_read))
        .route("/test/calls-write", get(calls_write))
        .route("/test/resolve", post(resolve_principal))
        .route("/test/limits", get(limits_handler))
}

/// The hex secret part of a raw credential (`cfmk_<env>_<public>_<secret>`).
pub fn secret_part(token: &str) -> &str {
    token.rsplit('_').next().unwrap()
}

/// Every row of every Calling table, as text.
pub async fn dump_database(conn: &DatabaseConnection) -> String {
    let tables: Vec<String> = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect();
    let mut out = String::new();
    for table in tables {
        let rows = conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT t::text FROM {table} t"),
            ))
            .await
            .unwrap();
        for row in rows {
            out.push_str(&row.try_get_by_index::<String>(0).unwrap());
            out.push('\n');
        }
    }
    out
}

pub async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    conn.execute_unprepared(sql).await.map(|_| ())
}

pub async fn scalar_i64(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index::<i64>(0)
    .unwrap()
}
