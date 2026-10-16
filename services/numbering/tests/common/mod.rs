//! Shared harness for Numbering integration tests.
//!
//! Requires PostgreSQL (no Docker): `TEST_DATABASE_URL` is an admin URL, e.g.
//! `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Each test gets its own database
//! cloned from a freshly migrated template and dropped afterwards. Without
//! `TEST_DATABASE_URL` the tests FAIL unless `CFMEET_SKIP_DB_TESTS=1` (explicit SKIPPED).
//!
//! Customer-facing tests start a REAL Platform Access (`PLATFORM_ACCESS_BIN`,
//! `PLATFORM_ACCESS_MIGRATION_BIN`) with its own database: Numbering authenticates
//! customers through it exactly as in production. Requests go through the real router
//! with real Ed25519 service tokens from independent peer identities.
#![allow(dead_code)]

pub mod platform_access;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use numbering::{
    config::{service_auth::ServiceAuthRuntime, WorkerSettings},
    domain::{ManualClock, RequestContext},
    http,
    identity::{CustomerAuthority, PlatformAccessAuthority, UnconfiguredAuthority},
    numbers::{
        assignments::{AssignmentRow, NewAssignment},
        bindings::NewBinding,
        capabilities::NewCapability,
        inventory::NewObservation,
        lifecycle::TransitionRequest,
        reservations::ReservationRow,
        resources::{NewResource, ResourceFacts, ResourceRow},
        routing::RoutingTargets,
        Actor, BindingChangeReason, Capability, CapabilitySource, EnvironmentKind, InventorySource,
        LifecycleState, NumberDomain, OwnerScope, OwnershipClass, SmsTargetKind, VoiceTargetKind,
    },
    state::AppState,
};
use numbering_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

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
                    "TEST_DATABASE_URL is not set. Numbering integration tests require PostgreSQL \
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

/// Drops databases of test processes that no longer exist (`nb_t_<pid>_…`).
async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    if !std::path::Path::new("/proc/self").exists() {
        return;
    }
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'nb\\_t\\_%' OR datname LIKE 'nb\\_tmpl\\_%'",
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
            .trim_start_matches("nb_tmpl_")
            .trim_start_matches("nb_t_")
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
            let name = format!("nb_tmpl_{}_{}", std::process::id(), Uuid::new_v4().simple());
            let conn = Database::connect(admin).await.expect("connect admin db");
            drop_databases_of_dead_runs(&conn).await;
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            numbering_migration::Migrator::up(&tmpl, None)
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
    /// Whether this handle created (and drops) the database.
    owned: bool,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        let name = format!("nb_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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
        let name = format!("nb_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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
        opts.max_connections(12).sqlx_logging(false);
        let conn = Database::connect(opts).await.unwrap();
        Self {
            conn,
            url,
            name,
            admin,
            owned: true,
        }
    }

    /// A handle on an existing connection (e.g. as another role); never drops anything.
    pub fn attach(conn: DatabaseConnection, url: String) -> Self {
        Self {
            conn,
            url,
            name: String::new(),
            admin: String::new(),
            owned: false,
        }
    }

    pub fn admin_url(&self) -> &str {
        &self.admin
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
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

/// Fixed start of the manual clock.
pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2031-03-01T10:00:30Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

pub fn trust_bundle_json(
    receiver: &str,
    grants: &[(&str, Vec<cf_service_auth::PublicJwk>, &[&str])],
) -> String {
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": ENV,
        "trusted_issuers": grants.iter().map(|(issuer, keys, permissions)| json!({
            "issuer": issuer,
            "audience": receiver,
            "runtime_environment": ENV,
            "keys": keys,
            "permissions": permissions,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// Peer identities that call Numbering.
pub struct Peers {
    pub calling: Arc<ServiceTokenSigner>,
    pub telephony: Arc<ServiceTokenSigner>,
    /// Correctly signing, but not in Numbering's trust bundle.
    pub privacy_relay: Arc<ServiceTokenSigner>,
    /// Trusted Calling identity name, but a key the bundle does not list.
    pub rogue_calling: Arc<ServiceTokenSigner>,
}

/// Numbering's service-auth runtime (system clock) whose client reaches `targets`.
pub fn service_auth(
    numbering_key: ServiceSigningKey,
    targets: &[(ServiceId, &str)],
) -> (ServiceAuthRuntime, Peers) {
    let clock: SharedClock = Arc::new(SystemClock);
    let calling = key("calling-test-1");
    let telephony = key("telephony-test-1");
    let policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::NUMBERING,
            &[
                (
                    cf_service_auth::services::CALLING,
                    vec![calling.public_jwk()],
                    &["number-resource.read"],
                ),
                (
                    cf_service_auth::services::TELEPHONY,
                    vec![telephony.public_jwk()],
                    &["number-resource.read"],
                ),
            ],
        ),
        &ServiceId::numbering(),
        ENV,
    )
    .unwrap();
    let signer = |id: ServiceId, k: ServiceSigningKey| {
        Arc::new(ServiceTokenSigner::new(
            id,
            ENV,
            k,
            TokenTtl::default(),
            clock.clone(),
        ))
    };
    let me = signer(ServiceId::numbering(), numbering_key);
    let mut t = InternalTargets::new(ENV);
    for (service, url) in targets {
        t.insert(service.clone(), url).unwrap();
    }
    let client = Arc::new(
        InternalServiceClient::new(me.clone(), t, std::time::Duration::from_secs(10)).unwrap(),
    );
    let runtime = ServiceAuthRuntime::new(
        me,
        Arc::new(ServiceTokenVerifier::new(policy, clock.clone())),
        client,
    )
    .unwrap();
    let peers = Peers {
        calling: signer(ServiceId::calling(), calling),
        telephony: signer(ServiceId::telephony(), telephony),
        privacy_relay: signer(ServiceId::privacy_relay(), key("privacy-relay-test-1")),
        rogue_calling: signer(ServiceId::calling(), key("calling-rogue-1")),
    };
    (runtime, peers)
}

/// Response of a harness request.
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

pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    pub numbers: NumberDomain,
    pub state: AppState,
    pub app: Router,
    pub peers: Peers,
    pub pa: Option<Arc<platform_access::PlatformAccess>>,
}

impl Harness {
    /// Domain + router over a fresh database; customer authentication is unconfigured
    /// (fails closed).
    pub async fn new() -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        Some(Self::build(db, None, ManualClock::new(t0())))
    }

    /// With a real Platform Access authenticating customers (wall-clock domain).
    pub async fn with_platform_access() -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        let pa = Arc::new(platform_access::PlatformAccess::start(db.admin_url()).await?);
        Some(Self::build(db, Some(pa), ManualClock::new(Utc::now())))
    }

    pub fn build(
        db: Arc<TestDb>,
        pa: Option<Arc<platform_access::PlatformAccess>>,
        clock: ManualClock,
    ) -> Self {
        let numbering_key = pa
            .as_ref()
            .map(|p| p.numbering_key())
            .unwrap_or_else(|| key("numbering-test-1"));
        let targets: Vec<(ServiceId, String)> = pa
            .as_ref()
            .map(|p| vec![(ServiceId::platform_access(), p.base_url.clone())])
            .unwrap_or_default();
        let target_refs: Vec<(ServiceId, &str)> = targets
            .iter()
            .map(|(s, u)| (s.clone(), u.as_str()))
            .collect();
        let (service_auth, peers) = service_auth(numbering_key, &target_refs);
        let authority: Arc<dyn CustomerAuthority> = match &pa {
            Some(_) => Arc::new(PlatformAccessAuthority::new(service_auth.client())),
            None => Arc::new(UnconfiguredAuthority),
        };
        let state = AppState::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            authority,
            service_auth,
            WorkerSettings::default(),
        );
        Self {
            numbers: state.numbers.clone(),
            app: http::app(state.clone()),
            db,
            clock,
            state,
            peers,
            pa,
        }
    }

    pub fn conn(&self) -> &DatabaseConnection {
        &self.db.conn
    }

    pub fn now(&self) -> DateTime<Utc> {
        use numbering::domain::Clock;
        self.clock.now()
    }

    /// A raw request through the real router.
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

    /// A signed internal request from `signer`.
    pub async fn internal(&self, signer: &ServiceTokenSigner, method: Method, path: &str) -> Reply {
        let signed = signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::numbering(),
            ))
            .unwrap();
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in signed.headers().iter() {
            builder = builder.header(name, value);
        }
        self.raw(builder.body(Body::empty()).unwrap()).await
    }

    /// A customer request with `Authorization: Bearer <token>`.
    pub async fn customer(&self, token: Option<&str>, method: Method, path: &str) -> Reply {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            builder = builder.header("authorization", format!("Bearer {t}"));
        }
        self.raw(builder.body(Body::empty()).unwrap()).await
    }

    /// Discovers a fresh, AVAILABLE number of `source` (synthetic numbers in the reserved
    /// +1 555 01xx range, real ones from a live provider).
    pub async fn available_number(&self, source: InventorySource) -> ResourceRow {
        let ctx = RequestContext::generate();
        let now = self.now();
        let number = random_number(source);
        let mut observation = NewObservation::offered(source, now, chrono::Duration::hours(1));
        if source == InventorySource::LiveProvider {
            observation.provider_id = Some("acme-telecom".into());
        }
        let row = self
            .numbers
            .discover(
                &ctx,
                Actor::system("inventory-test"),
                NewResource {
                    number,
                    country: if source == InventorySource::SyntheticTest {
                        "US".into()
                    } else {
                        "NG".into()
                    },
                    inventory_source: source,
                    classification: None,
                    facts: ResourceFacts::default(),
                },
                observation,
            )
            .await
            .unwrap();
        self.numbers
            .transition(
                &ctx,
                row.id,
                TransitionRequest::new(
                    LifecycleState::Available,
                    row.revision,
                    Actor::system("inventory-test"),
                    "OFFERED",
                ),
            )
            .await
            .unwrap()
    }

    pub async fn scalar(&self, sql: &str) -> i64 {
        scalar(self.conn(), sql).await
    }
}

pub async fn scalar(conn: &DatabaseConnection, sql: &str) -> i64 {
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

pub async fn strings(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}

pub async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    conn.execute_unprepared(sql).await.map(|_| ())
}

/// A unique number: synthetic ones in +1 555 01xx-style test ranges, real-looking Nigerian
/// mobile-range ones for live inventory.
pub fn random_number(source: InventorySource) -> String {
    let n = Uuid::new_v4().as_u128();
    match source {
        InventorySource::SyntheticTest => format!("+1555{:07}", n % 10_000_000),
        _ => format!("+234809{:07}", n % 10_000_000),
    }
}

/// A trusted owner scope (as Platform Access would establish).
pub fn scope(kind: EnvironmentKind) -> OwnerScope {
    OwnerScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        environment_kind: kind,
        service_account_id: Uuid::new_v4(),
    }
}

/// The inventory class a scope's environment may use.
pub fn source_for(scope: &OwnerScope) -> InventorySource {
    match scope.environment_kind {
        EnvironmentKind::Production => InventorySource::LiveProvider,
        _ => InventorySource::SyntheticTest,
    }
}

pub fn actor() -> Actor {
    Actor::system("numbering-tests")
}

pub fn reconciler() -> Actor {
    Actor::reconciliation("reconciliation-tests")
}

/// A binding of the inventory class's provider.
pub fn binding_for(source: InventorySource, capabilities: &[Capability]) -> NewBinding {
    NewBinding {
        provider_id: if source == InventorySource::SyntheticTest {
            "synthetic".into()
        } else {
            "acme-telecom".into()
        },
        external_ref: format!("PN{}", Uuid::new_v4().simple()),
        capabilities: capabilities.to_vec(),
        change_reason: BindingChangeReason::Initial,
    }
}

pub fn capability_source(source: InventorySource) -> CapabilitySource {
    if source == InventorySource::SyntheticTest {
        CapabilitySource::Synthetic
    } else {
        CapabilitySource::ProviderInventory
    }
}

pub fn voice_to_principal() -> RoutingTargets {
    RoutingTargets {
        voice: (VoiceTargetKind::Principal, Some(Uuid::new_v4())),
        sms: (SmsTargetKind::None, None),
    }
}

/// Each stage of the canonical path to an ACTIVE assignment.
pub struct Journey {
    pub resource_id: Uuid,
    pub reservation: ReservationRow,
    pub assignment: AssignmentRow,
    pub routing_profile_id: Uuid,
}

impl Harness {
    /// A number of the scope's inventory class, declared VOICE_INBOUND, reserved for
    /// `scope` with a confirmed provider hold (resource RESERVED, reservation HELD).
    pub async fn held(&self, scope: &OwnerScope) -> (ResourceRow, ReservationRow) {
        let ctx = RequestContext::generate();
        let source = source_for(scope);
        let resource = self.available_number(source).await;
        self.numbers
            .record_capability(
                &ctx,
                resource.id,
                NewCapability::declared(Capability::VoiceInbound, capability_source(source)),
            )
            .await
            .unwrap();
        let reservation = self
            .numbers
            .reserve(
                &ctx,
                scope,
                actor(),
                resource.id,
                chrono::Duration::minutes(10),
                None,
            )
            .await
            .unwrap();
        let reservation = self
            .numbers
            .confirm_hold(&ctx, actor(), reservation.id, Uuid::new_v4())
            .await
            .unwrap();
        (
            self.numbers.resource(resource.id).await.unwrap(),
            reservation,
        )
    }

    /// REQUESTED → … → RESERVED with a pinned routing profile (resource RESERVED).
    pub async fn reserved_assignment(&self, scope: &OwnerScope, class: OwnershipClass) -> Journey {
        let ctx = RequestContext::generate();
        let (resource, reservation) = self.held(scope).await;
        let n = &self.numbers;
        let a = n
            .request_assignment(
                &ctx,
                scope,
                actor(),
                NewAssignment {
                    number_resource_id: resource.id,
                    ownership_class: class,
                    owner_ref: Uuid::new_v4(),
                    policy_snapshot_ref: None,
                    kyc_profile_ref: None,
                },
            )
            .await
            .unwrap();
        let a = n
            .submit_for_eligibility(&ctx, actor(), a.id, a.revision)
            .await
            .unwrap();
        let a = n
            .record_eligibility(
                &ctx,
                actor(),
                a.id,
                a.revision,
                eligible_decision(self.conn(), scope, self.now()).await,
            )
            .await
            .unwrap();
        let a = n
            .attach_reservation(&ctx, actor(), a.id, a.revision, reservation.id)
            .await
            .unwrap();
        let profile = n
            .create_routing_profile(&ctx, scope, actor(), voice_to_principal())
            .await
            .unwrap();
        let a = n
            .set_assignment_routing(&ctx, actor(), a.id, a.revision, profile.profile_id)
            .await
            .unwrap();
        Journey {
            resource_id: resource.id,
            reservation,
            assignment: a,
            routing_profile_id: profile.profile_id,
        }
    }

    /// Moves the resource of `j` RESERVED → PROVISIONING → PROVISIONED (with a binding
    /// covering VOICE_INBOUND) and the assignment to ACTIVATING.
    pub async fn provisioned(&self, scope: &OwnerScope, j: &mut Journey) {
        let ctx = RequestContext::generate();
        let n = &self.numbers;
        let r = n.resource(j.resource_id).await.unwrap();
        n.transition(
            &ctx,
            r.id,
            TransitionRequest::new(
                LifecycleState::Provisioning,
                r.revision,
                actor(),
                "PROVISION_REQUESTED",
            ),
        )
        .await
        .unwrap();
        n.activate_binding(
            &ctx,
            r.id,
            binding_for(source_for(scope), &[Capability::VoiceInbound]),
        )
        .await
        .unwrap();
        let r = n.resource(j.resource_id).await.unwrap();
        n.transition(
            &ctx,
            r.id,
            TransitionRequest::new(
                LifecycleState::Provisioned,
                r.revision,
                actor(),
                "PROVISIONED",
            ),
        )
        .await
        .unwrap();
        j.assignment = n
            .begin_activation(&ctx, actor(), j.assignment.id, j.assignment.revision)
            .await
            .unwrap();
    }

    /// The whole canonical path: an ACTIVE assignment on an ACTIVE number.
    pub async fn active(&self, scope: &OwnerScope, class: OwnershipClass) -> Journey {
        let mut j = self.reserved_assignment(scope, class).await;
        self.provisioned(scope, &mut j).await;
        j.assignment = self
            .numbers
            .activate_assignment(
                &RequestContext::generate(),
                actor(),
                j.assignment.id,
                j.assignment.revision,
            )
            .await
            .unwrap();
        j
    }

    pub async fn outbox_count(&self) -> i64 {
        self.scalar("SELECT count(*) FROM numbering_outbox_events")
            .await
    }
}

/// Runs `statements` in one transaction (committed if all succeed, rolled back on the
/// first error, which is returned). Raw `BEGIN … COMMIT` strings would leave a pooled
/// connection in an aborted transaction.
/// A recorded ELIGIBLE decision of `scope` (fixture: the D journeys predate the
/// eligibility engine; F's own tests evaluate real decisions).
pub async fn eligible_decision(
    conn: &DatabaseConnection,
    scope: &OwnerScope,
    now: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::new_v4();
    let class = if scope.environment_kind == EnvironmentKind::Production {
        "REAL"
    } else {
        "SYNTHETIC"
    };
    exec(
        conn,
        &format!(
            "INSERT INTO number_eligibility_decisions (id, tenant_id, application_id, environment_id, environment_kind, \
               service_account_id, subject_kind, subject_ref, country_code, inventory_class, number_type, ownership_class, \
               requested_capabilities, outcome, reasons, catalog_revision, policy_version, evaluated_at, valid_until, \
               correlation_id) VALUES ('{id}', '{}', '{}', '{}', '{}', '{}', 'ORGANIZATION', gen_random_uuid(), 'US', '{class}', \
               'GEOGRAPHIC', 'CAMPAIGN', ARRAY['VOICE_INBOUND'], 'ELIGIBLE', '[]', 0, 'fixture', '{}'::timestamptz - interval '1 minute', \
               '{}'::timestamptz + interval '30 days', 'corr_fixture01')",
            scope.tenant_id,
            scope.application_id,
            scope.environment_id,
            scope.environment_kind.as_str(),
            scope.service_account_id,
            now.to_rfc3339(),
            now.to_rfc3339(),
        ),
    )
    .await
    .expect("insert eligibility fixture");
    id
}

pub async fn in_txn(conn: &DatabaseConnection, statements: &[&str]) -> Result<(), sea_orm::DbErr> {
    use sea_orm::TransactionTrait;
    let txn = conn.begin().await?;
    for sql in statements {
        if let Err(e) = txn.execute_unprepared(sql).await {
            txn.rollback().await.ok();
            return Err(e);
        }
    }
    txn.commit().await
}

/// Validators of registered contracts (`packages/contracts`), with every registered JSON
/// Schema resolvable by `$id`.
pub mod contracts {
    use jsonschema::{Registry, Resource, Validator};
    use serde_json::{json, Value};

    pub const BASE: &str = "https://schemas.cfmeet.com/";

    pub fn dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts")
    }

    pub fn read(rel: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir().join(rel)).unwrap()).unwrap()
    }

    fn registry() -> Registry<'static> {
        let manifest = read("manifest.json");
        let mut builder = Registry::new();
        for c in manifest["contracts"].as_array().unwrap() {
            if c["kind"] == "json-schema" {
                let doc = read(c["path"].as_str().unwrap());
                let id = doc["$id"].as_str().unwrap().to_string();
                builder = builder.add(id, Resource::from_contents(doc)).unwrap();
            }
        }
        builder.prepare().unwrap()
    }

    /// A validator of `reference` (e.g. `numbering/common/v1.schema.json#/$defs/Error`).
    pub fn validator(reference: &str) -> Validator {
        let registry = registry();
        jsonschema::options()
            .with_registry(&registry)
            .should_validate_formats(true)
            .build(&json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$ref": format!("{BASE}{reference}")
            }))
            .unwrap()
    }

    #[track_caller]
    pub fn conforms(reference: &str, value: &Value) {
        let v = validator(reference);
        let errors: Vec<String> = v
            .iter_errors(value)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{reference} violated: {errors:?}\n{value:#}"
        );
    }
}

/// Workstream E fixtures: catalogs and classified numbers.
pub mod catalog {
    use numbering::{
        domain::RequestContext,
        numbers::{
            catalog::{CatalogProvenance, CountryEntry, TypeEntry, TypeEntryRow},
            Actor, Capability, CatalogSource, ClassificationSource, CountrySupport, InventoryClass,
            InventorySource, LocalityClass, NumberType,
        },
    };
    use serde_json::json;
    use uuid::Uuid;

    use super::Harness;

    pub fn operator() -> Actor {
        Actor::operator("catalog-ops")
    }

    pub fn type_entry(
        country: &str,
        class: InventoryClass,
        number_type: NumberType,
        supported: &[Capability],
        unsupported: &[Capability],
    ) -> TypeEntry {
        TypeEntry {
            country: country.into(),
            inventory_class: class,
            number_type,
            locality: match number_type {
                NumberType::Geographic => LocalityClass::Local,
                NumberType::TollFree => LocalityClass::NonGeographic,
                _ => LocalityClass::National,
            },
            region: None,
            provider_id: None,
            supported: supported.to_vec(),
            unsupported: unsupported.to_vec(),
            limitations: json!({}),
            restriction_flags: vec![],
            jurisdiction_policy_ref: None,
            provenance: match class {
                InventoryClass::Synthetic => CatalogProvenance::synthetic(),
                InventoryClass::Real => CatalogProvenance::verified(
                    CatalogSource::ProviderDocumentation,
                    Uuid::new_v4(),
                ),
            },
        }
    }

    use Capability as C;

    /// The synthetic US test catalog: mobile (voice + SMS + mobile class), geographic
    /// (voice + inbound SMS + local), toll-free (inbound voice + business; SMS excluded).
    pub async fn synthetic_us(h: &Harness) -> Vec<TypeEntryRow> {
        let ctx = RequestContext::generate();
        h.numbers
            .publish_country(
                &ctx,
                operator(),
                "SYNTHETIC_CATALOG",
                CountryEntry {
                    country: "US".into(),
                    inventory_class: InventoryClass::Synthetic,
                    support: CountrySupport::Supported,
                    restriction_flags: vec![],
                    jurisdiction_policy_ref: None,
                    provenance: CatalogProvenance::synthetic(),
                },
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        for entry in [
            type_entry(
                "US",
                InventoryClass::Synthetic,
                NumberType::Mobile,
                &[
                    C::VoiceInbound,
                    C::VoiceOutbound,
                    C::SmsInbound,
                    C::SmsOutbound,
                    C::MobileNumberClass,
                ],
                &[C::MmsRich],
            ),
            type_entry(
                "US",
                InventoryClass::Synthetic,
                NumberType::Geographic,
                &[
                    C::VoiceInbound,
                    C::VoiceOutbound,
                    C::SmsInbound,
                    C::GeographicLocal,
                ],
                &[C::SmsOutbound, C::MobileNumberClass],
            ),
            type_entry(
                "US",
                InventoryClass::Synthetic,
                NumberType::TollFree,
                &[C::VoiceInbound, C::TollFreeBusiness],
                &[C::SmsInbound, C::SmsOutbound, C::VoiceOutbound],
            ),
        ] {
            out.push(
                h.numbers
                    .publish_number_type(&ctx, operator(), "SYNTHETIC_CATALOG", entry)
                    .await
                    .unwrap(),
            );
        }
        out
    }

    /// A VERIFIED real NG catalog (evidence refs stand for regulator / provider documents).
    pub async fn verified_ng(h: &Harness) -> Vec<TypeEntryRow> {
        let ctx = RequestContext::generate();
        h.numbers
            .publish_country(
                &ctx,
                operator(),
                "LAUNCH_COUNTRY",
                CountryEntry {
                    country: "NG".into(),
                    inventory_class: InventoryClass::Real,
                    support: CountrySupport::Supported,
                    restriction_flags: vec![],
                    jurisdiction_policy_ref: None,
                    provenance: CatalogProvenance::verified(
                        CatalogSource::RegulatorPublication,
                        Uuid::new_v4(),
                    ),
                },
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        for entry in [
            type_entry(
                "NG",
                InventoryClass::Real,
                NumberType::Mobile,
                &[
                    C::VoiceInbound,
                    C::VoiceOutbound,
                    C::SmsInbound,
                    C::SmsOutbound,
                    C::MobileNumberClass,
                ],
                &[],
            ),
            type_entry(
                "NG",
                InventoryClass::Real,
                NumberType::Geographic,
                &[C::VoiceInbound, C::VoiceOutbound, C::GeographicLocal],
                &[C::SmsOutbound],
            ),
        ] {
            out.push(
                h.numbers
                    .publish_number_type(&ctx, operator(), "LAUNCH_COUNTRY", entry)
                    .await
                    .unwrap(),
            );
        }
        out
    }

    /// An AVAILABLE number of `source`, classified as `number_type` from evidence.
    pub async fn classified(
        h: &Harness,
        source: InventorySource,
        number_type: NumberType,
    ) -> numbering::numbers::resources::ResourceRow {
        let r = h.available_number(source).await;
        let class_source = if source == InventorySource::SyntheticTest {
            ClassificationSource::Synthetic
        } else {
            ClassificationSource::ProviderInventory
        };
        h.numbers
            .classify(
                &RequestContext::generate(),
                r.id,
                r.revision,
                number_type,
                class_source,
            )
            .await
            .unwrap()
    }
}

impl Harness {
    /// A customer JSON request (`Authorization: Bearer`, optional `X-CF-Operator`).
    pub async fn customer_json(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        operator: Option<&str>,
    ) -> Reply {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"));
        if let Some(op) = operator {
            builder = builder.header("x-cf-operator", op);
        }
        let body = match body {
            Some(b) => {
                builder = builder.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        self.raw(builder.body(body).unwrap()).await
    }
}

impl Harness {
    /// The same harness with another KYC service and eligibility policy (router rebuilt).
    pub fn with_kyc(
        mut self,
        kyc: numbering::numbers::kyc::KycService,
        policy: numbering::numbers::eligibility::EligibilityPolicy,
    ) -> Self {
        self.state = self.state.clone().with_kyc(kyc, policy);
        self.app = http::app(self.state.clone());
        self
    }
}

/// Workstream F fixtures: a protected in-memory evidence vault, verifiers and the
/// synthetic US requirement sets.
pub mod kyc {
    use std::sync::Arc;

    use cf_object_storage::MemoryObjectStore;
    use cf_webhooks::secret_box::{SealingKey, SecretBox};
    use numbering::{
        domain::RequestContext,
        numbers::{
            kyc::{
                evidence::EvidenceVault,
                requirements::{RequirementItem, RequirementSet},
                verifier::KycVerifier,
                EvidenceKind, KycService, KycSettings, RequirementScope, RequirementType,
            },
            Actor, CatalogSource, InventoryClass, NumberType, OwnershipClass,
        },
    };

    use super::Harness;

    /// Test-only evidence key (bytes 0..31), never used outside tests.
    pub const TEST_EVIDENCE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    pub fn vault() -> (Arc<MemoryObjectStore>, Arc<EvidenceVault>) {
        let store = Arc::new(MemoryObjectStore::new());
        let sealing = SecretBox::new(
            SealingKey::from_base64("kyc-test-1", TEST_EVIDENCE_KEY).unwrap(),
            None,
        )
        .unwrap();
        let vault = Arc::new(EvidenceVault::new(store.clone(), Some(Arc::new(sealing))));
        (store, vault)
    }

    pub fn service(
        h: &Harness,
        vault: Arc<EvidenceVault>,
        verifier: Arc<dyn KycVerifier>,
    ) -> KycService {
        KycService::new(h.numbers.clone(), vault, verifier, KycSettings::default())
    }

    pub fn item(t: RequirementType, mandatory: bool, kinds: &[EvidenceKind]) -> RequirementItem {
        RequirementItem {
            requirement_type: t,
            mandatory,
            accepted_evidence: kinds.to_vec(),
        }
    }

    pub fn set(
        number_type: Option<NumberType>,
        ownership: Option<OwnershipClass>,
        items: Vec<RequirementItem>,
        validity: i32,
        revalidation: Option<i32>,
    ) -> RequirementSet {
        RequirementSet {
            scope: RequirementScope::Base,
            country: "US".into(),
            inventory_class: InventoryClass::Synthetic,
            number_type,
            ownership_class: ownership,
            provider_id: None,
            tenant_classification: None,
            kyc_mandatory: !items.is_empty(),
            items,
            approval_validity_days: validity,
            revalidation_days: revalidation,
            source: CatalogSource::SyntheticTest,
            source_version: "synthetic-2026.10".into(),
            effective_from: None,
            effective_to: None,
        }
    }

    pub fn operator() -> Actor {
        Actor::operator("kyc-reviewer")
    }

    /// The synthetic US requirements: no KYC by default; MOBILE / PERSONAL needs identity
    /// (address optional); MOBILE / ORGANIZATION needs a business registration and a purpose.
    pub async fn synthetic_us(h: &Harness) {
        use EvidenceKind as K;
        use RequirementType as T;
        let ctx = RequestContext::generate();
        for s in [
            set(None, None, vec![], 365, None),
            set(
                Some(NumberType::Mobile),
                Some(OwnershipClass::Personal),
                vec![
                    item(
                        T::IndividualIdentity,
                        true,
                        &[K::IdentityDocument, K::Passport],
                    ),
                    item(T::Address, false, &[K::ProofOfAddress]),
                ],
                365,
                Some(180),
            ),
            set(
                Some(NumberType::Mobile),
                Some(OwnershipClass::Organization),
                vec![
                    item(
                        T::BusinessRegistration,
                        true,
                        &[K::BusinessRegistrationCertificate],
                    ),
                    item(T::PurposeOfUse, true, &[K::PurposeStatement]),
                ],
                365,
                None,
            ),
        ] {
            h.numbers
                .publish_requirements(&ctx, operator(), "SYNTHETIC_POLICY", s)
                .await
                .unwrap();
        }
    }

    /// A minimal PDF carrying `marker` (to prove it never leaks).
    pub fn pdf(marker: &str) -> Vec<u8> {
        format!("%PDF-1.7\n% {marker}\n%%EOF\n").into_bytes()
    }
}
