//! Shared harness for SMS Gateway integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL; each test gets
//! its own database cloned from a freshly migrated template. Without it the tests FAIL,
//! unless `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED line).
//!
//! Requests go through the REAL router. Customer credentials are resolved by a scripted
//! authority (the real Platform Access is exercised by the cross-service journeys); a
//! gateway-forwarded customer request carries a real API Gateway service token (trusted
//! with exactly `sms-request.forward`) whose delegated credential id the scripted
//! authority re-verifies like Platform Access's delegated-authority operation;
//! Numbering is a scripted port; the provider is the deterministic adapter; internal
//! requests carry real Ed25519 service tokens verified by the gateway's trust bundle.
#![allow(dead_code)]

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
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    CorrelationId, DelegatedPlatformContext, KeyId, OutboundServiceRequest, RuntimeEnvironment,
    ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier, SharedClock,
    SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use sms_gateway::{
    config::{service_auth::ServiceAuthRuntime, ControlSettings, TransportSettings},
    domain::ManualClock,
    http,
    identity::{
        Authentication, AuthorityUnavailable, CustomerAuthority, EnvironmentKind, OwnerScope,
        PrincipalDirectory, PrincipalStatus, TrustedContext,
    },
    numbering::{
        NumberingError, NumberingPort, ReleasedSender, Scope, SenderState, SmsDestination,
        SmsRoute, VerifiedRouting,
    },
    providers::deterministic::DeterministicSmsProvider,
    sms::protection::Protection,
    state::AppState,
};
use sms_gateway_migration::MigratorTrait;
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;
use zeroize::Zeroizing;

static TEMPLATE: OnceCell<String> = OnceCell::const_new();
static CREATE_LOCK: Mutex<()> = Mutex::const_new(());

pub const CALLBACK_SECRET: &[u8] = b"sms-test-callback-secret-0123456789abcdef";
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
                    "TEST_DATABASE_URL is not set. SMS Gateway integration tests require PostgreSQL \
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
            "SELECT datname FROM pg_database WHERE datname LIKE 'sg\\_t\\_%' OR datname LIKE 'sg\\_tmpl\\_%'",
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
            .trim_start_matches("sg_tmpl_")
            .trim_start_matches("sg_t_")
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
            let name = format!("sg_tmpl_{}_{}", std::process::id(), Uuid::new_v4().simple());
            let conn = Database::connect(admin).await.expect("connect admin db");
            drop_databases_of_dead_runs(&conn).await;
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            sms_gateway_migration::Migrator::up(&tmpl, None)
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
        let name = format!("sg_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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

pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2031-03-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub fn key_bytes(seed: u8) -> Vec<u8> {
    (0..32u8)
        .map(|i| i.wrapping_mul(seed).wrapping_add(seed))
        .collect()
}

pub fn protection() -> Protection {
    Protection::new("content-1", &key_bytes(7), None, &key_bytes(11)).unwrap()
}

// ---------------------------------------------------------------------------------------
// Customers
// ---------------------------------------------------------------------------------------

/// A scripted Platform Access: token -> trusted context.
#[derive(Default)]
pub struct FakeAuthority {
    pub tokens: StdMutex<HashMap<String, TrustedContext>>,
    pub down: StdMutex<bool>,
    /// Every delegated context a delegated-authority request carried (with its correlation).
    pub delegated: StdMutex<Vec<(DelegatedPlatformContext, CorrelationId)>>,
    /// Answers the next delegated-authority request with this chain, unchecked (a
    /// misbehaving authority).
    pub misbind: StdMutex<Option<TrustedContext>>,
}

#[async_trait]
impl CustomerAuthority for FakeAuthority {
    async fn authenticate(&self, presented: &str) -> Result<Authentication, AuthorityUnavailable> {
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

    /// Platform Access's delegated-authority operation: the LIVE chain of the delegated
    /// credential, only when it belongs to the delegated tenant / application /
    /// environment / service account.
    async fn authenticate_delegated(
        &self,
        delegated: &DelegatedPlatformContext,
        correlation_id: &CorrelationId,
    ) -> Result<Authentication, AuthorityUnavailable> {
        self.delegated
            .lock()
            .unwrap()
            .push((delegated.clone(), correlation_id.clone()));
        if *self.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        if let Some(chain) = self.misbind.lock().unwrap().take() {
            return Ok(Authentication::Authenticated(Box::new(chain)));
        }
        let live = self
            .tokens
            .lock()
            .unwrap()
            .values()
            .find(|c| Some(c.credential_id) == delegated.initiating_credential_id())
            .cloned();
        Ok(match live {
            Some(c)
                if c.tenant_id == delegated.tenant_id()
                    && c.application_id == delegated.application_id()
                    && c.environment_id == delegated.application_environment_id()
                    && c.service_account_id == delegated.initiating_service_account_id() =>
            {
                Authentication::Authenticated(Box::new(c))
            }
            _ => Authentication::Rejected {
                reason: "unknown_credential".into(),
                throttle: None,
            },
        })
    }
}

/// A scripted principal directory (ACTIVE unless listed otherwise).
#[derive(Default)]
pub struct FakePrincipals {
    pub inactive: StdMutex<BTreeSet<Uuid>>,
}

#[async_trait]
impl PrincipalDirectory for FakePrincipals {
    async fn resolve(
        &self,
        _scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        Ok(if self.inactive.lock().unwrap().contains(&principal_id) {
            PrincipalStatus::Inactive
        } else {
            PrincipalStatus::Active
        })
    }
}

/// One customer environment and a credential for it.
#[derive(Clone)]
pub struct Customer {
    pub token: String,
    pub context: TrustedContext,
}

impl Customer {
    pub fn scope(&self) -> Scope {
        Scope {
            tenant_id: self.context.tenant_id,
            application_id: self.context.application_id,
            environment_id: self.context.environment_id,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Numbering
// ---------------------------------------------------------------------------------------

/// One number Numbering knows (what its records say).
#[derive(Clone)]
pub struct FakeNumber {
    pub id: Uuid,
    pub e164: String,
    pub scope: Scope,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    /// SMS routing target; None = NOT_ROUTED.
    pub sms_target: Option<(String, Uuid)>,
    /// Some(reason) = UNAVAILABLE (not active, released, …).
    pub unavailable: Option<String>,
    /// Who may send from it (None = anyone of the scope; Some(p) = only principal p).
    pub sender_principal: Option<Uuid>,
    pub outbound_sms: bool,
}

#[derive(Default)]
pub struct FakeNumbering {
    pub down: StdMutex<bool>,
    pub numbers: StdMutex<Vec<FakeNumber>>,
    /// (operation ref, number) of every released sender (single use per operation).
    pub released: StdMutex<Vec<(String, Uuid)>>,
    pub routes: StdMutex<HashMap<Uuid, SmsRoute>>,
    pub acknowledged: StdMutex<Vec<(Uuid, SmsRoute)>>,
    pub resolutions: StdMutex<usize>,
    /// Inbound grants issued: token -> (grant id, the number as resolved).
    pub inbound_grants: StdMutex<HashMap<String, (Uuid, FakeNumber)>>,
    pub verifications: StdMutex<usize>,
    pub sender_checks: StdMutex<usize>,
    /// Numbering refuses the next sender grants with this reason (expired, revoked, …).
    pub refuse_sender: StdMutex<Option<String>>,
    /// Numbering unreachable for grant verification only.
    pub verify_down: StdMutex<bool>,
    /// Runs once, right after the next resolution (a routing change racing delivery).
    #[allow(clippy::type_complexity)]
    pub after_resolve: StdMutex<Option<Box<dyn FnOnce(&mut Vec<FakeNumber>) + Send>>>,
}

impl FakeNumbering {
    pub fn add(&self, n: FakeNumber) {
        self.numbers.lock().unwrap().push(n);
    }

    pub fn update(&self, id: Uuid, f: impl FnOnce(&mut FakeNumber)) {
        let mut numbers = self.numbers.lock().unwrap();
        f(numbers.iter_mut().find(|n| n.id == id).unwrap());
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }
}

#[async_trait]
impl NumberingPort for FakeNumbering {
    async fn resolve_destination(
        &self,
        destination: &str,
        _: &CorrelationId,
    ) -> Result<SmsDestination, NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        *self.resolutions.lock().unwrap() += 1;
        let numbers = self.numbers.lock().unwrap();
        let Some(n) = numbers.iter().find(|n| n.e164 == destination) else {
            return Ok(SmsDestination::Unavailable {
                reason: "UNKNOWN_NUMBER".into(),
            });
        };
        if let Some(reason) = &n.unavailable {
            return Ok(SmsDestination::Unavailable {
                reason: reason.clone(),
            });
        }
        let out = match &n.sms_target {
            None => SmsDestination::NotRouted {
                reason: "NOT_ROUTED".into(),
            },
            Some((kind, target)) => {
                let grant_id = Uuid::new_v4();
                let token = format!("fake-inbound-grant.{}", Uuid::new_v4().simple());
                self.inbound_grants
                    .lock()
                    .unwrap()
                    .insert(token.clone(), (grant_id, n.clone()));
                SmsDestination::Routed {
                    number_resource_id: n.id,
                    assignment_id: n.assignment_id,
                    routing_revision: n.routing_revision,
                    scope: n.scope,
                    target_kind: kind.clone(),
                    target_ref: *target,
                    grant_id,
                    grant: Zeroizing::new(token),
                }
            }
        };
        drop(numbers);
        if let Some(hook) = self.after_resolve.lock().unwrap().take() {
            hook(&mut self.numbers.lock().unwrap());
        }
        Ok(out)
    }

    async fn verify_inbound_sms(
        &self,
        grant: &str,
        _: &CorrelationId,
    ) -> Result<VerifiedRouting, NumberingError> {
        if *self.down.lock().unwrap() || *self.verify_down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        *self.verifications.lock().unwrap() += 1;
        let (grant_id, then) = self
            .inbound_grants
            .lock()
            .unwrap()
            .get(grant)
            .cloned()
            .ok_or(NumberingError::Refused("GRANT_INVALID".into()))?;
        // Verified against CURRENT state: any change since issuance revokes the grant.
        let numbers = self.numbers.lock().unwrap();
        let now = numbers
            .iter()
            .find(|n| n.id == then.id)
            .ok_or(NumberingError::Refused("GRANT_REVOKED".into()))?;
        if now.unavailable.is_some()
            || now.assignment_id != then.assignment_id
            || now.routing_revision != then.routing_revision
            || now.scope != then.scope
            || now.sms_target != then.sms_target
        {
            return Err(NumberingError::Refused("GRANT_REVOKED".into()));
        }
        Ok(VerifiedRouting {
            grant_id,
            number_resource_id: now.id,
            assignment_id: now.assignment_id,
            routing_revision: now.routing_revision,
            scope: now.scope,
            target: now.sms_target.clone(),
        })
    }

    async fn release_sender(
        &self,
        delegated: &DelegatedPlatformContext,
        _: &CorrelationId,
        number_resource_id: Uuid,
        operation_ref: &str,
    ) -> Result<ReleasedSender, NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        let numbers = self.numbers.lock().unwrap();
        let n = numbers
            .iter()
            .find(|n| {
                n.id == number_resource_id
                    && n.scope.tenant_id == delegated.tenant_id()
                    && n.scope.application_id == delegated.application_id()
                    && n.scope.environment_id == delegated.application_environment_id()
            })
            .ok_or(NumberingError::Refused("NOT_FOUND".into()))?;
        if n.unavailable.is_some() || !n.outbound_sms {
            return Err(NumberingError::Refused("PRECONDITION_FAILED".into()));
        }
        if let Some(reason) = self.refuse_sender.lock().unwrap().clone() {
            return Err(NumberingError::Refused(reason));
        }
        if let Some(p) = n.sender_principal {
            if delegated.initiating_principal().map(|i| i.principal_id) != Some(p) {
                return Err(NumberingError::Refused("PRECONDITION_FAILED".into()));
            }
        }
        let mut released = self.released.lock().unwrap();
        if released.iter().any(|(op, _)| op == operation_ref) {
            return Err(NumberingError::Refused("CONFLICT".into()));
        }
        released.push((operation_ref.to_string(), n.id));
        Ok(ReleasedSender {
            e164: Zeroizing::new(n.e164.clone()),
            number_resource_id: n.id,
            assignment_id: n.assignment_id,
            routing_revision: n.routing_revision,
            grant_id: Uuid::new_v4(),
            scope: n.scope,
        })
    }

    async fn sender_current(
        &self,
        delegated: &DelegatedPlatformContext,
        _: &CorrelationId,
        number_resource_id: Uuid,
    ) -> Result<SenderState, NumberingError> {
        if *self.down.lock().unwrap() || *self.verify_down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        *self.sender_checks.lock().unwrap() += 1;
        let numbers = self.numbers.lock().unwrap();
        let n = numbers
            .iter()
            .find(|n| {
                n.id == number_resource_id
                    && n.scope.tenant_id == delegated.tenant_id()
                    && n.scope.application_id == delegated.application_id()
                    && n.scope.environment_id == delegated.application_environment_id()
            })
            .ok_or(NumberingError::Refused("NOT_FOUND".into()))?;
        Ok(SenderState {
            assignment_id: n.assignment_id,
            active: n.unavailable.is_none() && n.outbound_sms,
        })
    }

    async fn sms_route(
        &self,
        number_resource_id: Uuid,
    ) -> Result<Option<SmsRoute>, NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        Ok(self
            .routes
            .lock()
            .unwrap()
            .get(&number_resource_id)
            .cloned())
    }

    async fn acknowledge_sms_route(
        &self,
        number_resource_id: Uuid,
        route: &SmsRoute,
    ) -> Result<(), NumberingError> {
        if *self.down.lock().unwrap() {
            return Err(NumberingError::Unavailable);
        }
        self.acknowledged
            .lock()
            .unwrap()
            .push((number_resource_id, route.clone()));
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Service identities
// ---------------------------------------------------------------------------------------

pub fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

pub struct Peers {
    /// Correctly signing, but not in the gateway's trust bundle.
    pub telephony: Arc<ServiceTokenSigner>,
    /// Messaging: trusted with exactly `sms-bridge.submit` (Part V H SMS Bridge).
    pub messaging: Arc<ServiceTokenSigner>,
    /// The API Gateway: trusted with exactly `sms-request.forward` (Part VI §51).
    pub api_gateway: Arc<ServiceTokenSigner>,
    /// Claims the API Gateway identity, but signs with a key the bundle does not hold.
    pub rogue_api_gateway: Arc<ServiceTokenSigner>,
}

/// The gateway's service-auth runtime: Messaging is trusted with exactly
/// `sms-bridge.submit`, the API Gateway with exactly `sms-request.forward`.
pub fn service_auth() -> (ServiceAuthRuntime, Peers) {
    let clock: SharedClock = Arc::new(SystemClock);
    let me = key("sms-gateway-test-1");
    let telephony = key("telephony-test-1");
    let messaging = key("messaging-test-1");
    let api_gateway = key("api-gateway-test-1");
    let rogue_api_gateway = key("api-gateway-test-2");
    let bundle = json!({
        "bundle_version": 1,
        "receiver": cf_service_auth::services::SMS_GATEWAY,
        "runtime_environment": ENV,
        "trusted_issuers": [{
            "issuer": cf_service_auth::services::MESSAGING,
            "audience": cf_service_auth::services::SMS_GATEWAY,
            "runtime_environment": ENV,
            "keys": [messaging.public_jwk()],
            "permissions": ["sms-bridge.submit"],
        }, {
            "issuer": cf_service_auth::services::API_GATEWAY,
            "audience": cf_service_auth::services::SMS_GATEWAY,
            "runtime_environment": ENV,
            "keys": [api_gateway.public_jwk()],
            "permissions": ["sms-request.forward"],
        }],
    })
    .to_string();
    let policy = TrustPolicy::from_json(&bundle, &ServiceId::sms_gateway(), ENV).unwrap();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::sms_gateway(),
        ENV,
        me,
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
    let peers = Peers {
        telephony: Arc::new(ServiceTokenSigner::new(
            ServiceId::telephony(),
            ENV,
            telephony,
            TokenTtl::default(),
            clock.clone(),
        )),
        messaging: Arc::new(ServiceTokenSigner::new(
            ServiceId::messaging(),
            ENV,
            messaging,
            TokenTtl::default(),
            clock.clone(),
        )),
        api_gateway: Arc::new(ServiceTokenSigner::new(
            ServiceId::api_gateway(),
            ENV,
            api_gateway,
            TokenTtl::seconds(60).unwrap(),
            clock.clone(),
        )),
        rogue_api_gateway: Arc::new(ServiceTokenSigner::new(
            ServiceId::api_gateway(),
            ENV,
            rogue_api_gateway,
            TokenTtl::seconds(60).unwrap(),
            clock,
        )),
    };
    (runtime, peers)
}

// ---------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------

pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub json: Value,
    pub text: String,
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
    pub numbering: Arc<FakeNumbering>,
    pub provider: DeterministicSmsProvider,
    pub peers: Peers,
    pub state: AppState,
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::build(|_| {}).await
    }

    /// A harness whose state is adjusted by `configure` (controls, transport settings).
    pub async fn build(configure: impl FnOnce(&mut AppState)) -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        Some(Self::on(db, configure))
    }

    pub fn on(db: Arc<TestDb>, configure: impl FnOnce(&mut AppState)) -> Self {
        let clock = ManualClock::new(t0());
        let authority = Arc::new(FakeAuthority::default());
        let principals = Arc::new(FakePrincipals::default());
        let numbering = Arc::new(FakeNumbering::default());
        let provider = DeterministicSmsProvider::new("deterministic", CALLBACK_SECRET)
            .with_stall(std::time::Duration::from_millis(400));
        let (runtime, peers) = service_auth();
        let mut state = AppState::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            authority.clone(),
            runtime,
            protection(),
        )
        .with_numbering(numbering.clone())
        .with_principals(principals.clone())
        .with_provider(Arc::new(provider.clone()))
        .with_transport(TransportSettings {
            submit_timeout: std::time::Duration::from_millis(150),
            ..TransportSettings::default()
        })
        .with_controls(ControlSettings {
            allowed_prefixes: vec!["+1".into(), "+44".into(), "+234".into()],
            ..ControlSettings::default()
        })
        .with_instance("sms-test-1");
        configure(&mut state);
        Self {
            db,
            clock,
            authority,
            principals,
            numbering,
            provider,
            peers,
            state,
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        use sms_gateway::domain::Clock;
        self.clock.now()
    }

    pub fn advance(&self, seconds: i64) {
        self.clock.advance(chrono::Duration::seconds(seconds));
    }

    pub fn conn(&self) -> &DatabaseConnection {
        &self.db.conn
    }

    pub fn router(&self) -> Router {
        http::app(self.state.clone())
    }

    /// A customer environment with `scopes`.
    pub fn customer(&self, scopes: &[&str]) -> Customer {
        let context = TrustedContext {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: EnvironmentKind::Sandbox,
            service_account_id: Uuid::new_v4(),
            credential_id: Uuid::new_v4(),
            first_party: false,
            live_telecom_allowed: true,
            full_authority: true,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        };
        let token = format!("cfk_test_{}", Uuid::new_v4().simple());
        self.authority
            .tokens
            .lock()
            .unwrap()
            .insert(token.clone(), context.clone());
        Customer { token, context }
    }

    /// Another credential of the same environment with other scopes.
    pub fn credential(&self, of: &Customer, scopes: &[&str]) -> Customer {
        let mut context = of.context.clone();
        context.credential_id = Uuid::new_v4();
        context.scopes = scopes.iter().map(|s| s.to_string()).collect();
        let token = format!("cfk_test_{}", Uuid::new_v4().simple());
        self.authority
            .tokens
            .lock()
            .unwrap()
            .insert(token.clone(), context.clone());
        Customer { token, context }
    }

    /// A number of `customer` Numbering routes SMS to (`target`).
    pub fn number(
        &self,
        customer: &Customer,
        e164: &str,
        target: Option<(&str, Uuid)>,
    ) -> FakeNumber {
        let n = FakeNumber {
            id: Uuid::new_v4(),
            e164: e164.to_string(),
            scope: customer.scope(),
            assignment_id: Uuid::new_v4(),
            routing_revision: 1,
            sms_target: target.map(|(k, r)| (k.to_string(), r)),
            unavailable: None,
            sender_principal: None,
            outbound_sms: true,
        };
        self.numbering.add(n.clone());
        n
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
        }
    }

    pub async fn get(&self, token: Option<&str>, path: &str) -> Response {
        let mut b = Request::builder().method(Method::GET).uri(path);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        self.raw(b.body(Body::empty()).unwrap()).await
    }

    pub async fn post(
        &self,
        token: Option<&str>,
        path: &str,
        body: Value,
        idempotency_key: Option<&str>,
    ) -> Response {
        let mut b = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if let Some(k) = idempotency_key {
            b = b.header("idempotency-key", k);
        }
        self.raw(b.body(Body::from(body.to_string())).unwrap())
            .await
    }

    /// A provider callback (signed headers + raw body) to `path`.
    pub async fn callback(&self, path: &str, headers: HeaderMap, body: Vec<u8>) -> Response {
        let mut b = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("content-type", "application/json");
        for (k, v) in headers.iter() {
            b = b.header(k, v);
        }
        self.raw(b.body(Body::from(body)).unwrap()).await
    }

    /// `POST /v1/sms/messages` with `key`.
    pub async fn send(&self, c: &Customer, body: Value, key: &str) -> Response {
        self.post(Some(&c.token), "/v1/sms/messages", body, Some(key))
            .await
    }

    /// One dispatcher pass.
    pub async fn dispatch(&self) -> sms_gateway::sms::dispatch::DispatchReport {
        sms_gateway::sms::dispatch::run_once(&self.state)
            .await
            .unwrap()
    }

    /// Posts a signed delivery receipt.
    pub async fn receipt(&self, signed: &(HeaderMap, Vec<u8>)) -> Response {
        self.callback(
            "/provider-callbacks/v1/sms/deterministic/delivery-receipts",
            signed.0.clone(),
            signed.1.clone(),
        )
        .await
    }

    /// A signed receipt for `reference` at the gateway's now.
    pub fn receipt_callback(
        &self,
        reference: &str,
        status: sms_gateway::providers::ReceiptStatus,
        error: Option<&str>,
    ) -> (HeaderMap, Vec<u8>) {
        self.provider
            .receipt_callback(reference, status, error, self.now())
    }

    /// A signed inbound-message callback of the deterministic provider at the gateway's now.
    pub fn inbound_callback(&self, from: &str, to: &str, text: &str) -> (HeaderMap, Vec<u8>) {
        self.provider.inbound_callback(from, to, text, self.now())
    }

    /// Posts an inbound callback (headers + raw body).
    pub async fn inbound(&self, signed: &(HeaderMap, Vec<u8>)) -> Response {
        self.callback(
            "/provider-callbacks/v1/sms/deterministic/inbound",
            signed.0.clone(),
            signed.1.clone(),
        )
        .await
    }

    /// An internal JSON call signed by `signer`, optionally with a delegated scope.
    pub async fn internal_call(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        delegated: Option<cf_service_auth::DelegatedPlatformContext>,
    ) -> Response {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::sms_gateway());
        if let Some(d) = delegated {
            outbound = outbound.with_delegated_context(d);
        }
        let signed = signer.sign(&outbound).unwrap();
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
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

    /// The delegated context the API Gateway signs for a request it authenticated with
    /// `customer`'s credential.
    pub fn gateway_delegation(&self, customer: &Customer) -> DelegatedPlatformContext {
        customer
            .context
            .owner_scope()
            .delegated(None)
            .with_initiating_credential(customer.context.credential_id)
    }

    /// A customer request as the API Gateway forwards it: the original method, path,
    /// body and `idempotency-key`, NO `Authorization` header, and a fresh service token of
    /// `signer` carrying `delegated` (plus any `extra` headers, e.g. an Authorization a
    /// test adds on purpose). Returns the response and the signed correlation id.
    #[allow(clippy::too_many_arguments)]
    pub async fn forwarded(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        body: Option<Value>,
        idempotency_key: Option<&str>,
        delegated: Option<DelegatedPlatformContext>,
        extra: &[(&str, &str)],
    ) -> (Response, CorrelationId) {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::sms_gateway());
        if let Some(d) = delegated {
            outbound = outbound.with_delegated_context(d);
        }
        let correlation = outbound.correlation_id().clone();
        let signed = signer.sign(&outbound).unwrap();
        let mut b = Request::builder().method(method).uri(path);
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
        if let Some(k) = idempotency_key {
            b = b.header("idempotency-key", k);
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
        (self.raw(b.body(body).unwrap()).await, correlation)
    }

    /// An internal request signed by `signer`.
    pub async fn internal(&self, signer: &ServiceTokenSigner, path: &str) -> Response {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::sms_gateway(),
            ))
            .unwrap();
        let mut b = Request::builder().method(Method::GET).uri(path);
        for (k, v) in signed.headers().iter() {
            b = b.header(k, v);
        }
        self.raw(b.body(Body::empty()).unwrap()).await
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

    pub async fn text(&self, sql: &str) -> Option<String> {
        self.conn()
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap()
            .and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
    }

    pub async fn sql(&self, sql: &str) -> Result<(), String> {
        self.conn()
            .execute_unprepared(sql)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Every text column of every table (canary scans).
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

/// Validates `value` against a schema of the served OpenAPI's `components.schemas`.
#[track_caller]
pub fn conforms_component(name: &str, value: &Value) {
    let api: Value = serde_json::from_str(sms_gateway::http::OPENAPI).unwrap();
    let schema = api["components"]["schemas"][name].clone();
    assert!(schema.is_object(), "no component {name}");
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{name} violated: {errors:?}\n{value:#}");
}
