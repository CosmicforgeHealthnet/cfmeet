//! Shared harness for meeting-service integration tests.
//!
//! Requires PostgreSQL (no Docker needed): set `TEST_DATABASE_URL` to an admin URL,
//! e.g. `postgres://postgres@127.0.0.1:5432/postgres`. Each test gets its own
//! database cloned from a freshly migrated template. If `TEST_DATABASE_URL` is unset
//! the tests FAIL, unless `CFMEET_SKIP_DB_TESTS=1` is set — then they print an explicit
//! SKIPPED line (never a silent pass).
#![allow(dead_code)]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use backend::{
    config::{AppConfig, EmailConfig, FeatureFlags, LiveKitConfig, MeetingV2Config, RedisConfig},
    domain::Role,
    models::users,
    routes,
    services::{
        actor::StaticOrganizationDirectory,
        meeting_core::{
            AccessAdapterRegistry, AccessAdapterRequest, AdapterDecision, ExternalAccessAdapter,
        },
        meeting_domain::MeetingDomainDeps,
        meeting_permissions::MediaPermissions,
        rtc_provider::{
            JoinCredentialRequest, JoinCredentials, RoomInfo, RoomSpec, RtcError, RtcParticipant,
            RtcProvider, RtcResult,
        },
        RedisService,
    },
    state::AppState,
};
use chrono::Utc;
use migration::MigratorTrait;
use sea_orm::{ActiveModelTrait, ConnectionTrait, Database, DatabaseConnection, Set, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

fn admin_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") {
                eprintln!("SKIPPED: database integration test (CFMEET_SKIP_DB_TESTS=1, TEST_DATABASE_URL unset)");
                None
            } else {
                panic!(
                    "TEST_DATABASE_URL is not set. Database integration/security/concurrency tests require \
                     PostgreSQL (e.g. TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres). \
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

/// Test databases are named `cfm_t_<pid>_…` / `cfm_tmpl_<pid>_…`. Databases left behind
/// by test processes that no longer exist (panics, the per-binary template) are dropped.
/// Liveness is checked through `/proc`; where it is unavailable nothing is dropped.
async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    if !std::path::Path::new("/proc/self").exists() {
        return;
    }
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'cfm\\_t\\_%' OR datname LIKE 'cfm\\_tmpl\\_%'".to_string(),
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
            .trim_start_matches("cfm_tmpl_")
            .trim_start_matches("cfm_t_")
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
                "cfm_tmpl_{}_{}",
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
            migration::Migrator::up(&tmpl, None)
                .await
                .expect("migrations apply");
            tmpl.close().await.ok();
            name
        })
        .await
        .clone()
}

/// An isolated, migrated database for one test. Dropped (FORCE) on drop.
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
        let name = format!("cfm_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        {
            let _guard = CREATE_LOCK.lock().await;
            let conn = Database::connect(&admin).await.unwrap();
            conn.execute_unprepared(&format!("CREATE DATABASE {name} TEMPLATE {tmpl}"))
                .await
                .unwrap();
            conn.close().await.ok();
        }
        let url = with_db(&admin, &name);
        let mut opts = sea_orm::ConnectOptions::new(url.clone());
        opts.max_connections(40).sqlx_logging(false);
        let conn = Database::connect(opts).await.unwrap();
        Some(Self {
            conn,
            url,
            name,
            admin,
        })
    }

    /// Empty database (no migrations) for migration tests.
    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        let name = format!("cfm_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
        let conn = Database::connect(&admin).await.unwrap();
        conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        conn.close().await.ok();
        let url = with_db(&admin, &name);
        let conn = Database::connect(&url).await.unwrap();
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

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// Records provider interactions; issues obviously fake (test-only) credentials.
#[derive(Default)]
pub struct FakeRtc {
    pub issued: AtomicUsize,
    pub other_calls: AtomicUsize,
    pub fail_issue: std::sync::atomic::AtomicBool,
    pub last_permissions: std::sync::Mutex<Option<MediaPermissions>>,
}

#[async_trait]
impl RtcProvider for FakeRtc {
    fn name(&self) -> &'static str {
        "FAKE"
    }
    async fn create_room(&self, _s: &RoomSpec) -> RtcResult<RoomInfo> {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        Err(RtcError::NotSupported("fake"))
    }
    async fn get_room(&self, _r: &str) -> RtcResult<Option<RoomInfo>> {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    async fn terminate_room(&self, _r: &str) -> RtcResult<()> {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn issue_join_credentials(
        &self,
        req: &JoinCredentialRequest,
    ) -> RtcResult<JoinCredentials> {
        if self.fail_issue.load(Ordering::SeqCst) {
            return Err(RtcError::Unavailable("fake outage".into()));
        }
        self.issued.fetch_add(1, Ordering::SeqCst);
        *self.last_permissions.lock().unwrap() = Some(req.permissions);
        Ok(JoinCredentials {
            provider: "LIVEKIT",
            url: "wss://rtc.test".into(),
            credential: format!("test-credential-for-{}", req.media_identity),
            expires_at: Utc::now() + chrono::Duration::from_std(req.ttl).unwrap(),
        })
    }
    async fn remove_participant(&self, _r: &str, _m: &str) -> RtcResult<()> {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn update_permissions(&self, _r: &str, _m: &str, _p: &MediaPermissions) -> RtcResult<()> {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn mute_track(&self, _r: &str, _m: &str, _t: &str, _x: bool) -> RtcResult<()> {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn list_participants(&self, _r: &str) -> RtcResult<Vec<RtcParticipant>> {
        Ok(vec![])
    }
    async fn start_recording(&self, _r: &str) -> RtcResult<String> {
        Err(RtcError::NotSupported("fake"))
    }
    async fn stop_recording(&self, _r: &str, _id: &str) -> RtcResult<()> {
        Err(RtcError::NotSupported("fake"))
    }
    async fn reconcile(&self, _r: &str) -> RtcResult<Option<RoomInfo>> {
        Ok(None)
    }
}

/// Healthcare appointment adapter: allows only listed users.
pub struct AllowListAdapter(pub Vec<Uuid>);

#[async_trait]
impl ExternalAccessAdapter for AllowListAdapter {
    async fn evaluate(&self, req: &AccessAdapterRequest) -> AdapterDecision {
        match req.user_id {
            Some(u) if self.0.contains(&u) => AdapterDecision::Allow { role: None },
            _ => AdapterDecision::Deny,
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct TestOptions {
    pub flags: FeatureFlags,
    pub organizations: HashMap<Uuid, Vec<Uuid>>,
    pub healthcare_allow: Vec<Uuid>,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            flags: FeatureFlags {
                rtc_provider_control_v2: true,
                ..FeatureFlags::all_v2_enabled()
            },
            organizations: HashMap::new(),
            healthcare_allow: vec![],
        }
    }
}

pub struct TestApp {
    pub db: TestDb,
    pub state: AppState,
    pub router: Router,
    pub rtc: Arc<FakeRtc>,
}

pub struct User {
    pub id: Uuid,
    pub token: String,
    pub email: String,
}

#[derive(Clone, Copy)]
pub enum Auth<'a> {
    None,
    Bearer(&'a str),
    Participant(&'a str),
}

impl TestApp {
    pub async fn new() -> Option<Self> {
        Self::with(TestOptions::default()).await
    }

    pub async fn with(opts: TestOptions) -> Option<Self> {
        let db = TestDb::new().await?;
        Some(Self::on(db, opts))
    }

    pub fn on(db: TestDb, opts: TestOptions) -> Self {
        let app_config = AppConfig {
            database_url: db.url.clone(),
            jwt_secret: "test-jwt-secret".into(),
            host: "127.0.0.1".into(),
            port: 0,
            frontend_urls: vec![],
            app_url: "http://localhost:3000".into(),
            google_client_id: String::new(),
            google_client_secret: String::new(),
            google_redirect_url: String::new(),
            require_email_verification: true,
        };
        let livekit = LiveKitConfig {
            url: "wss://rtc.test".into(),
            api_key: "k".into(),
            api_secret: "s".into(),
        };
        // Redis deliberately unreachable: v2 authority must not depend on it.
        let redis = RedisService::new_lazy(&RedisConfig {
            url: std::env::var("TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:1".into()),
            pool_size: 2,
            timeout_secs: 1,
        })
        .unwrap();
        let rtc = Arc::new(FakeRtc::default());
        let mut adapters = AccessAdapterRegistry::default();
        adapters.register(
            "healthnet-appointments",
            Arc::new(AllowListAdapter(opts.healthcare_allow.clone())),
        );
        let deps = MeetingDomainDeps {
            flags: opts.flags,
            config: MeetingV2Config {
                passcode_hash_cost: 4,
                passcode_max_attempts: 5,
                ..MeetingV2Config::default()
            },
            organizations: Arc::new(StaticOrganizationDirectory {
                memberships: opts.organizations,
            }),
            rtc: rtc.clone(),
            adapters,
        };
        let state = AppState::with_meeting_deps(
            db.conn.clone(),
            redis,
            &app_config,
            &EmailConfig::default(),
            &livekit,
            deps,
        );
        let router = routes::create_routes(state.clone());
        Self {
            db,
            state,
            router,
            rtc,
        }
    }

    pub async fn user(&self, name: &str) -> User {
        self.user_with_email(
            name,
            &format!(
                "{name}-{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
        )
        .await
    }

    pub async fn user_with_email(&self, name: &str, email: &str) -> User {
        let now = Utc::now().naive_utc();
        let id = Uuid::new_v4();
        users::ActiveModel {
            id: Set(id),
            username: Set(format!("{name}-{}", &id.simple().to_string()[..6])),
            email: Set(email.to_string()),
            password_hash: Set(None),
            auth_type: Set(users::AuthType::Local),
            role: Set(users::UserRole::User),
            status: Set(users::UserStatus::Active),
            created_at: Set(now),
            updated_at: Set(now),
            last_login: Set(None),
        }
        .insert(&self.db.conn)
        .await
        .unwrap();
        let token = self
            .state
            .jwt_service
            .generate_access_token(id, "user")
            .unwrap();
        User {
            id,
            token,
            email: email.to_string(),
        }
    }

    pub async fn admin(&self, name: &str) -> User {
        let u = self.user(name).await;
        self.db
            .conn
            .execute(Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "UPDATE users SET role = 'admin' WHERE id = $1",
                [u.id.into()],
            ))
            .await
            .unwrap();
        u
    }

    pub async fn call(
        &self,
        method: Method,
        uri: &str,
        auth: Auth<'_>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        match auth {
            Auth::None => {}
            Auth::Bearer(t) => req = req.header("authorization", format!("Bearer {t}")),
            Auth::Participant(t) => req = req.header("x-participant-token", t),
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&b).unwrap()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = tokio::time::timeout(Duration::from_secs(30), self.router.clone().oneshot(req))
            .await
            .expect("request timed out")
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes) }))
        };
        (status, value)
    }

    pub async fn get(&self, uri: &str, auth: Auth<'_>) -> (StatusCode, Value) {
        self.call(Method::GET, uri, auth, None).await
    }

    pub async fn post(&self, uri: &str, auth: Auth<'_>, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, uri, auth, Some(body)).await
    }

    pub async fn patch(&self, uri: &str, auth: Auth<'_>, body: Value) -> (StatusCode, Value) {
        self.call(Method::PATCH, uri, auth, Some(body)).await
    }

    // -- scenario helpers -----------------------------------------------------

    pub async fn create_space(&self, owner: &User, body: Value) -> Value {
        let (s, v) = self
            .post("/api/v2/meeting-spaces", Auth::Bearer(&owner.token), body)
            .await;
        assert_eq!(s, StatusCode::CREATED, "create space: {v}");
        v
    }

    /// Space + LIVE conference. Returns (space_id, conference_id).
    pub async fn live_conference(&self, owner: &User, space_body: Value) -> (Uuid, Uuid) {
        let space = self.create_space(owner, space_body).await;
        let space_id = uuid(&space["id"]);
        let (s, c) = self
            .post(
                &format!("/api/v2/meeting-spaces/{space_id}/conferences"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "create conference: {c}");
        let conference_id = uuid(&c["id"]);
        let (s, v) = self
            .post(
                &format!("/api/v2/conferences/{conference_id}/start"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "start: {v}");
        (space_id, conference_id)
    }

    pub async fn join(
        &self,
        conference_id: Uuid,
        auth: Auth<'_>,
        body: Value,
    ) -> (StatusCode, Value) {
        self.post(
            &format!("/api/v2/conferences/{conference_id}/join"),
            auth,
            body,
        )
        .await
    }

    pub async fn add_member(&self, space_id: Uuid, by: &User, user: &User, role: Role) {
        let (s, v) = self
            .post(
                &format!("/api/v2/meeting-spaces/{space_id}/members"),
                Auth::Bearer(&by.token),
                json!({ "userId": user.id, "role": role.as_str() }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "add member: {v}");
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        let row = self
            .db
            .conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get_by_index::<i64>(0).unwrap()
    }
}

pub fn uuid(v: &Value) -> Uuid {
    Uuid::parse_str(
        v.as_str()
            .unwrap_or_else(|| panic!("expected uuid string, got {v}")),
    )
    .unwrap()
}

pub fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("<none>")
}
