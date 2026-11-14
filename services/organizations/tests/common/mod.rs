//! Shared harness for Organizations integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets
//! its own database cloned from a freshly migrated template. Without it the tests FAIL,
//! unless `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED line).
//!
//! Requests go through the REAL router with REAL Ed25519 service tokens verified by the
//! Organizations trust bundle (receiver-authoritative grants of `packages/service-auth`).
#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    Router,
};
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use http_body_util::BodyExt;
use organizations::{http, state::AppState};
use organizations_migration::MigratorTrait;
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
                    "TEST_DATABASE_URL is not set. Organizations integration tests require PostgreSQL \
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
            "SELECT datname FROM pg_database WHERE datname LIKE 'org\\_t\\_%' OR datname LIKE 'org\\_tmpl\\_%'",
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
            .trim_start_matches("org_tmpl_")
            .trim_start_matches("org_t_")
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
                "org_tmpl_{}_{}",
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
            organizations_migration::Migrator::up(&tmpl, None)
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
        let name = format!("org_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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

/// Callers of Organizations in tests (grants as the registry allows them).
pub struct Peers {
    /// The account relay: read, write, act-for-user, administer, migrate.
    pub meeting: Arc<ServiceTokenSigner>,
    /// Read only (no act-for-user).
    pub messaging: Arc<ServiceTokenSigner>,
    /// Operator tooling: administer, migrate.
    pub operator: Arc<ServiceTokenSigner>,
    /// A key nobody trusts, claiming to be the Meeting Service.
    pub rogue_meeting: Arc<ServiceTokenSigner>,
}

pub fn verifier_and_peers() -> (Arc<ServiceTokenVerifier>, Peers) {
    let clock: SharedClock = Arc::new(SystemClock);
    let meeting = key("meeting-test-1");
    let messaging = key("messaging-test-1");
    let operator = key("operator-test-1");
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str]| {
        json!({
            "issuer": id,
            "audience": cf_service_auth::services::ORGANIZATIONS,
            "runtime_environment": ENV,
            "keys": [k.public_jwk()],
            "permissions": perms,
        })
    };
    let bundle = json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::ORGANIZATIONS,
        "runtime_environment": ENV,
        "trusted_issuers": [
            issuer(cf_service_auth::services::MEETING_SERVICE, &meeting, &[
                "organization.read", "organization.write", "organization.act-for-user",
                "organization.administer", "organization.migrate",
            ]),
            issuer(cf_service_auth::services::MESSAGING, &messaging, &["organization.read"]),
            issuer(cf_service_auth::services::OPERATOR_CONSOLE, &operator, &[
                "organization.administer", "organization.migrate",
            ]),
        ],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::organizations(), ENV).unwrap();
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock));
    (
        verifier,
        Peers {
            meeting: signer(ServiceId::meeting_service(), meeting),
            messaging: signer(ServiceId::messaging(), messaging),
            operator: signer(ServiceId::operator_console(), operator),
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
        assert_eq!(self.headers.get("cache-control").unwrap(), "no-store");
        assert_eq!(self.json["schemaVersion"], 1);
    }
}

pub struct Harness {
    pub db: TestDb,
    pub router: Router,
    pub peers: Peers,
}

/// One signed request under construction.
pub struct Call<'a> {
    h: &'a Harness,
    signer: &'a ServiceTokenSigner,
    headers: HeaderMap,
}

impl<'a> Call<'a> {
    pub fn actor(mut self, user: Uuid) -> Self {
        self.headers.insert(
            "x-cf-acting-user",
            HeaderValue::from_str(&user.to_string()).unwrap(),
        );
        self
    }

    pub fn email(mut self, email: &str) -> Self {
        self.headers.insert(
            "x-cf-acting-user-verified-email",
            HeaderValue::from_str(email).unwrap(),
        );
        self
    }

    pub fn idem(mut self, key: &str) -> Self {
        self.headers
            .insert("idempotency-key", HeaderValue::from_str(key).unwrap());
        self
    }

    /// The signed headers (for replay tests).
    pub fn signed_headers(&self) -> HeaderMap {
        let signed = self
            .signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::organizations(),
            ))
            .unwrap();
        let mut headers = self.headers.clone();
        signed.apply_to(&mut headers);
        headers
    }

    pub async fn send(self, method: Method, path: &str, body: Option<Value>) -> Response {
        let headers = self.signed_headers();
        self.h.raw(method, path, headers, body).await
    }

    pub async fn get(self, path: &str) -> Response {
        self.send(Method::GET, path, None).await
    }

    pub async fn post(self, path: &str, body: Value) -> Response {
        self.send(Method::POST, path, Some(body)).await
    }

    pub async fn patch(self, path: &str, body: Value) -> Response {
        self.send(Method::PATCH, path, Some(body)).await
    }
}

impl Harness {
    pub async fn new() -> Option<Self> {
        let db = TestDb::new().await?;
        let (verifier, peers) = verifier_and_peers();
        let state = AppState::new(db.conn.clone(), verifier);
        Some(Self {
            router: http::app(state),
            db,
            peers,
        })
    }

    /// A harness whose HTTP state carries a Platform Access port (operator binding
    /// rotation, ADR 0058 A2).
    pub async fn with_platform(
        platform: std::sync::Arc<dyn organizations::provisioning::PlatformScopes>,
    ) -> Option<Self> {
        let db = TestDb::new().await?;
        let (verifier, peers) = verifier_and_peers();
        let state = AppState::new(db.conn.clone(), verifier)
            .with_provisioning(true)
            .with_platform(Some(platform));
        Some(Self {
            router: http::app(state),
            db,
            peers,
        })
    }

    pub fn by<'a>(&'a self, signer: &'a ServiceTokenSigner) -> Call<'a> {
        Call {
            h: self,
            signer,
            headers: HeaderMap::new(),
        }
    }

    /// The Meeting Service relaying for `user`.
    pub fn user(&self, user: Uuid) -> Call<'_> {
        self.by(&self.peers.meeting).actor(user)
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

    pub async fn exec(&self, sql: &str) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
        self.db
            .conn
            .execute(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
            .await
    }

    /// Creates an organization owned by `owner` (self-service) and returns its JSON.
    pub async fn create_org(&self, owner: Uuid, slug: &str) -> Value {
        self.user(owner)
            .post(
                "/internal/v1/organizations",
                json!({ "name": format!("Org {slug}"), "slug": slug }),
            )
            .await
            .expect(201)
            .clone()
    }

    /// Invites `email` with `role` (by `inviter`) and returns `(invitation, token)`.
    pub async fn invite(
        &self,
        org: &str,
        inviter: Uuid,
        email: &str,
        role: &str,
    ) -> (Value, String) {
        let r = self
            .user(inviter)
            .post(
                &format!("/internal/v1/organizations/{org}/invitations"),
                json!({ "email": email, "role": role }),
            )
            .await;
        let v = r.expect(201).clone();
        let token = v["token"].as_str().unwrap().to_string();
        (v, token)
    }

    /// Invites and accepts: `user` becomes a member with `role`; returns the membership.
    pub async fn add_member(&self, org: &str, inviter: Uuid, user: Uuid, role: &str) -> Value {
        let email = format!("{}@example.com", user.simple());
        let (_, token) = self.invite(org, inviter, &email, role).await;
        self.user(user)
            .email(&email)
            .post(
                "/internal/v1/organization-invitations/accept",
                json!({ "token": token }),
            )
            .await
            .expect(200)
            .clone()
    }

    /// The organization's current version (as the owner sees it).
    pub async fn version(&self, org: &str, viewer: Uuid) -> i64 {
        self.user(viewer)
            .get(&format!("/internal/v1/organizations/{org}"))
            .await
            .expect(200)["version"]
            .as_i64()
            .unwrap()
    }
}

pub fn s(v: &Value) -> String {
    v.as_str().unwrap().to_string()
}
