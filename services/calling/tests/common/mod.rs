//! Shared harness for Calling integration tests.
//!
//! Requires PostgreSQL (no Docker needed): set `TEST_DATABASE_URL` to an admin URL, e.g.
//! `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Each test gets its own
//! database cloned from a freshly migrated template and dropped afterwards. Without
//! `TEST_DATABASE_URL` the tests FAIL, unless `CFMEET_SKIP_DB_TESTS=1` is set — then they
//! print an explicit SKIPPED line (never a silent pass).
//!
//! Identity (tenants … credentials, principals) is owned by Platform Access: every
//! migrated test database comes with its own real Platform Access process
//! ([`identity::PlatformAccess`]), Calling authenticates through it exactly as in
//! production, and `Harness::platform` administers identity through it. Calling's own
//! state runs on a manually driven clock.
#![allow(dead_code)]

pub mod app;
pub mod calls;
pub mod contracts;
pub mod deployment;
pub mod enterprise;
pub mod fake_media;
pub mod gates;
pub mod identity;
pub mod load;
pub mod privacy;
pub mod provider_contract;
pub mod public;
pub mod telephony;

use std::{collections::HashMap, sync::Arc};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    routing::{get, post},
    Json, Router,
};
use calling::{
    calls::{app::AppCallingConfig, CallServiceConfig},
    domain::{
        EnvironmentKind, ManualClock, PrincipalId, PrincipalKind, RequestContext, ServiceAccountId,
        TenantClassification,
    },
    error::CallingError,
    internal_auth::{middleware::InternalCaller, ServiceAuthRuntime},
    media::{CallMediaProvider, MediaCredentialPolicy},
    platform::{
        applications::ApplicationView,
        authentication::AuthenticationError,
        context::PlatformAuthContext,
        environments::EnvironmentView,
        limits::LimitDefaults,
        principals::{PrincipalReference, PrincipalResolutionError},
        scopes::names,
        service_accounts::ServiceAccountView,
        tenants::TenantView,
        PlatformService,
    },
    routes,
    signaling::{InMemorySignalBus, NoPushTransport, RealtimeSignalBus, SignalingKeys},
    state::{AppState, CallingRuntime},
};
use calling_migration::MigratorTrait;
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

#[allow(unused_imports)]
pub use identity::{
    IssueCredential, IssuedCredential, NewApplication, NewPrincipal, NewServiceAccount, NewTenant,
    RotateCredential, TestPlatform, TEST_PEPPER,
};

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
pub(crate) static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

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

pub(crate) fn with_db(admin: &str, db: &str) -> String {
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
    /// The identity authority of this database's deployments (migrated databases only).
    pub platform_access: Option<Arc<identity::PlatformAccess>>,
    name: String,
    admin: String,
}

impl TestDb {
    /// Migrated database.
    pub async fn new() -> Option<Self> {
        Self::with_platform_access_env(&[]).await
    }

    /// Migrated database whose Platform Access runs with extra environment.
    pub async fn with_platform_access_env(pa_env: &[(&str, &str)]) -> Option<Self> {
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
        let platform_access = Arc::new(identity::PlatformAccess::start_with(&admin, pa_env).await?);
        let mut db = Self::connect(admin, name).await?;
        db.platform_access = Some(platform_access);
        Some(db)
    }

    /// The Platform Access of this database (migrated databases only).
    pub fn pa(&self) -> Arc<identity::PlatformAccess> {
        self.platform_access
            .clone()
            .expect("an empty (unmigrated) test database has no Platform Access")
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
            platform_access: None,
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

/// Fixed service-auth time (seconds), 2031-03-01T10:00:00Z.
pub const SERVICE_T0: i64 = 1_930_212_000;

/// Independent service identities that talk to Calling in tests (no deployable services
/// are created): their own keys and signers, in the `test` runtime environment.
pub struct Peers {
    pub clock: cf_service_auth::ManualClock,
    pub privacy_relay: Arc<ServiceTokenSigner>,
    pub telephony: Arc<ServiceTokenSigner>,
    /// The SMS Gateway: `platform-event.publish` only (Part IV N / O).
    pub sms_gateway: Arc<ServiceTokenSigner>,
    /// Messaging: `platform-event.publish` only (Part V I).
    pub messaging: Arc<ServiceTokenSigner>,
    /// A correctly signing service that Calling does NOT trust.
    pub untrusted: Arc<ServiceTokenSigner>,
    /// A verifier for a privacy-relay identity that trusts Calling (outbound tests).
    pub privacy_relay_verifier: Arc<ServiceTokenVerifier>,
}

fn service_key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

pub fn trust_bundle_json(
    receiver: &str,
    env: RuntimeEnvironment,
    grants: &[(&str, Vec<cf_service_auth::PublicJwk>, &[&str])],
) -> String {
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": env,
        "trusted_issuers": grants.iter().map(|(issuer, keys, permissions)| json!({
            "issuer": issuer,
            "audience": receiver,
            "runtime_environment": env,
            "keys": keys,
            "permissions": permissions,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// Calling's service-auth runtime (test runtime environment) and its peers.
pub fn service_auth_fixture() -> (ServiceAuthRuntime, Peers) {
    service_auth_fixture_targeting(&[])
}

/// [`service_auth_fixture`] whose outbound client reaches `targets` (service, base URL).
pub fn service_auth_fixture_targeting(
    targets: &[(ServiceId, &str)],
) -> (ServiceAuthRuntime, Peers) {
    let env = RuntimeEnvironment::Test;
    let clock = cf_service_auth::ManualClock::new(SERVICE_T0);
    let shared: cf_service_auth::SharedClock = Arc::new(clock.clone());
    let calling_key = service_key("calling-test-1");
    let relay_key = service_key("privacy-relay-test-1");
    let telephony_key = service_key("telephony-test-1");
    let untrusted_key = service_key("untrusted-test-1");
    let sms_gateway_key = service_key("sms-gateway-test-1");
    let messaging_key = service_key("messaging-test-1");
    let calling_jwk = calling_key.public_jwk();

    let calling_policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::CALLING,
            env,
            &[
                (
                    cf_service_auth::services::PRIVACY_RELAY,
                    vec![relay_key.public_jwk()],
                    &[
                        "operational-callback.deliver",
                        "communication-authority.enforce",
                        "platform-event.publish",
                    ],
                ),
                (
                    cf_service_auth::services::TELEPHONY,
                    vec![telephony_key.public_jwk()],
                    &[
                        "operational-callback.deliver",
                        "telephony-event.apply",
                        "telephony-reconciliation.apply",
                        "telephony-inbound.offer",
                        "telephony-enterprise.offer",
                        "telephony-capture.deliver",
                    ],
                ),
                (
                    cf_service_auth::services::SMS_GATEWAY,
                    vec![sms_gateway_key.public_jwk()],
                    &["platform-event.publish"],
                ),
                (
                    cf_service_auth::services::MESSAGING,
                    vec![messaging_key.public_jwk()],
                    &["platform-event.publish"],
                ),
            ],
        ),
        &ServiceId::calling(),
        env,
    )
    .unwrap();
    let relay_policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::PRIVACY_RELAY,
            env,
            &[(
                cf_service_auth::services::CALLING,
                vec![calling_jwk],
                &[
                    "communication.authorize",
                    "communication-context.read",
                    "communication-context.write",
                    "relay-session.read",
                    "relay-session.write",
                ],
            )],
        ),
        &ServiceId::privacy_relay(),
        env,
    )
    .unwrap();

    let calling_signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::calling(),
        env,
        calling_key,
        TokenTtl::default(),
        shared.clone(),
    ));
    let verifier = Arc::new(ServiceTokenVerifier::new(calling_policy, shared.clone()));
    let client = Arc::new(
        InternalServiceClient::new(
            calling_signer.clone(),
            {
                let mut t = InternalTargets::new(env);
                for (service, url) in targets {
                    t.insert(service.clone(), url).unwrap();
                }
                t
            },
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    );
    let runtime = ServiceAuthRuntime::new(calling_signer, verifier, client).unwrap();
    let peers = Peers {
        privacy_relay: Arc::new(ServiceTokenSigner::new(
            ServiceId::privacy_relay(),
            env,
            relay_key,
            TokenTtl::default(),
            shared.clone(),
        )),
        telephony: Arc::new(ServiceTokenSigner::new(
            ServiceId::telephony(),
            env,
            telephony_key,
            TokenTtl::default(),
            shared.clone(),
        )),
        sms_gateway: Arc::new(ServiceTokenSigner::new(
            ServiceId::sms_gateway(),
            env,
            sms_gateway_key,
            TokenTtl::default(),
            shared.clone(),
        )),
        messaging: Arc::new(ServiceTokenSigner::new(
            ServiceId::messaging(),
            env,
            messaging_key,
            TokenTtl::default(),
            shared.clone(),
        )),
        untrusted: Arc::new(ServiceTokenSigner::new(
            ServiceId::parse("cfmeet.numbering").unwrap(),
            env,
            untrusted_key,
            TokenTtl::default(),
            shared.clone(),
        )),
        privacy_relay_verifier: Arc::new(ServiceTokenVerifier::new(relay_policy, shared)),
        clock,
    };
    (runtime, peers)
}

/// Database + platform service + HTTP state wired with a manual clock.
pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    /// Identity administration (through Platform Access) and Calling-owned platform
    /// reads/limits (`Deref` to Calling's [`PlatformService`]).
    pub platform: TestPlatform,
    pub state: AppState,
    pub peers: Arc<Peers>,
    /// The in-memory media provider wired into `state` (unless a test chose another).
    pub media: Arc<fake_media::FakeMediaProvider>,
    /// The single-process signal bus wired into `state`.
    pub bus: Arc<InMemorySignalBus>,
    /// Push-ready offers Calling dispatched (recording transport).
    pub push: Arc<RecordingPush>,
}

/// Deterministic test-only signaling key (never used outside tests).
/// A push transport that records what Calling dispatched.
#[derive(Default)]
pub struct RecordingPush {
    sent: std::sync::Mutex<Vec<(uuid::Uuid, Value)>>,
}

impl RecordingPush {
    pub fn sent(&self) -> Vec<(uuid::Uuid, Value)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl calling::signaling::PushDispatcher for RecordingPush {
    async fn dispatch(&self, push_target_ref: uuid::Uuid, payload: Value) {
        self.sent.lock().unwrap().push((push_target_ref, payload));
    }
}

pub const TEST_SIGNALING_KEY: &str = "calling-test-signaling-key-4b8e2f6a1d9c3e7b5a0f";

pub fn test_signaling_keys() -> Arc<SignalingKeys> {
    Arc::new(
        SignalingKeys::from_lookup(&|k: &str| {
            (k == "CALLING_SIGNALING_SIGNING_KEY").then(|| TEST_SIGNALING_KEY.to_string())
        })
        .unwrap(),
    )
}

/// Calling runtime of a test deployment: test signaling key, the given media provider
/// and bus, default tunables.
pub fn test_runtime(
    media: Option<Arc<dyn CallMediaProvider>>,
    bus: Arc<dyn RealtimeSignalBus>,
) -> CallingRuntime {
    CallingRuntime {
        signaling_keys: test_signaling_keys(),
        media,
        media_gateway: None,
        credential_policy: MediaCredentialPolicy::default(),
        bus,
        push: Arc::new(NoPushTransport),
        app_config: AppCallingConfig::default(),
        call_config: CallServiceConfig::default(),
        privacy_relay: Arc::new(calling::privacy_relay::UnconfiguredPrivacyRelay),
        numbering: Arc::new(calling::numbering::UnconfiguredNumbering),
        contact_delegation: None,
        public_network: Default::default(),
        enterprise_flags: calling::enterprise::EnterpriseFlags::ALL_ON,
        artifacts: Arc::new(cf_object_storage::MemoryObjectStore::new()),
        capture: Arc::new(calling::calls::app::enterprise::UnavailableCapture),
        webhooks: calling::webhooks::WebhookSettings::default(),
        compliance_overlay: Default::default(),
        abuse_controls: Default::default(),
    }
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
    names::PRIVACY_CONTEXTS_READ,
    names::PRIVACY_CONTEXTS_WRITE,
    names::PRIVACY_SESSIONS_READ,
    names::PRIVACY_SESSIONS_WRITE,
    names::PRIVACY_CONTACTS_READ,
    names::PRIVACY_CONTACTS_WRITE,
    names::PRIVACY_CONTACTS_VERIFY,
    names::PRIVACY_EVIDENCE_READ,
];

impl Harness {
    pub async fn new() -> Option<Self> {
        let db = TestDb::new().await?;
        Some(Self::on(Arc::new(db), ManualClock::new(t0())))
    }

    /// Another Calling deployment over the same database and Platform Access.
    pub fn sibling(&self) -> Self {
        Self::on(self.db.clone(), self.clock.clone())
    }

    pub fn on(db: Arc<TestDb>, clock: ManualClock) -> Self {
        let media = Arc::new(fake_media::FakeMediaProvider::default());
        let bus = Arc::new(InMemorySignalBus::new());
        Self::with_runtime(
            db,
            clock,
            media.clone(),
            bus.clone(),
            test_runtime(Some(media), bus),
        )
    }

    /// A deployment with an explicit Calling runtime (real provider, other bus, config).
    pub fn with_runtime(
        db: Arc<TestDb>,
        clock: ManualClock,
        media: Arc<fake_media::FakeMediaProvider>,
        bus: Arc<InMemorySignalBus>,
        mut runtime: CallingRuntime,
    ) -> Self {
        let push = Arc::new(RecordingPush::default());
        runtime.push = push.clone();
        let (service_auth, peers) = service_auth_fixture();
        Self::with_service_auth(db, clock, media, bus, runtime, push, service_auth, peers)
    }

    /// A deployment with an explicit service-auth runtime (cross-service tests).
    #[allow(clippy::too_many_arguments)]
    pub fn with_service_auth(
        db: Arc<TestDb>,
        clock: ManualClock,
        media: Arc<fake_media::FakeMediaProvider>,
        bus: Arc<InMemorySignalBus>,
        runtime: CallingRuntime,
        push: Arc<RecordingPush>,
        service_auth: ServiceAuthRuntime,
        peers: Peers,
    ) -> Self {
        let pa = db.pa();
        let state = AppState::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            pa.authority(),
            LimitDefaults::default(),
            service_auth,
            runtime,
        );
        let platform = TestPlatform::new(
            pa,
            PlatformService::new(
                db.conn.clone(),
                Arc::new(clock.clone()),
                LimitDefaults::default(),
            ),
            state.identity.clone(),
        );
        Self {
            db,
            clock,
            platform,
            state,
            peers: Arc::new(peers),
            media,
            bus,
            push,
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
            .authenticate(token, RequestContext::generate())
            .await
    }

    pub async fn ctx(&self, token: &str) -> PlatformAuthContext {
        self.authenticate(token).await.expect("authenticates")
    }

    pub fn router(&self) -> Router {
        routes::app(
            self.state.clone(),
            test_routes().merge(routes::public_api()),
            test_internal_routes().merge(routes::internal_api()),
        )
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
        "correlation_id": ctx.correlation_id().as_str(),
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

// ---------------------------------------------------------------------------
// Test-only internal routes (mounted under /internal/v1 by routes::app).
// ---------------------------------------------------------------------------

pub fn internal_context_json(caller: &InternalCaller) -> Value {
    let c = caller.context();
    json!({
        "service": c.authenticated_service(),
        "audience": c.audience(),
        "runtime_environment": c.runtime_environment(),
        "kid": c.key_id().as_str(),
        "request_id": c.request_id(),
        "correlation_id": c.correlation_id(),
        "permissions": c.permissions().names(),
        "delegated_context": c.delegated_platform_context(),
        "current_request_id": calling::middleware::request_id::current_request_id().map(|r| r.as_str().to_string()),
    })
}

async fn internal_caller(caller: InternalCaller, _body: Option<Json<Value>>) -> Json<Value> {
    Json(internal_context_json(&caller))
}

async fn internal_callback(caller: InternalCaller) -> Result<Json<Value>, CallingError> {
    caller.require_permission(InternalPermission::OperationalCallbackDeliver)?;
    Ok(Json(json!({ "accepted": true })))
}

/// Requires a permission Calling can never grant (received by Privacy Relay only).
async fn internal_never_granted(caller: InternalCaller) -> Result<Json<Value>, CallingError> {
    caller.require_permission(InternalPermission::CommunicationAuthorize)?;
    Ok(Json(json!({ "unreachable": true })))
}

async fn internal_slow(_caller: InternalCaller) -> Json<Value> {
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    Json(json!({ "slow": true }))
}

/// Location for the redirect test route (set by the test that uses it).
pub static REDIRECT_TARGET: std::sync::OnceLock<String> = std::sync::OnceLock::new();

async fn internal_redirect(_caller: InternalCaller) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::TEMPORARY_REDIRECT,
        [(
            axum::http::header::LOCATION,
            REDIRECT_TARGET.get().cloned().unwrap_or_default(),
        )],
    )
        .into_response()
}

pub fn test_internal_routes() -> Router<AppState> {
    Router::new()
        .route("/test/caller", get(internal_caller).post(internal_caller))
        .route("/test/callback", post(internal_callback))
        .route("/test/never-granted", post(internal_never_granted))
        .route("/test/slow", get(internal_slow))
        .route("/test/redirect", get(internal_redirect))
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
