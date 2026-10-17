//! Part IV Step 20 — synthetic communications journeys across the ACTUAL service
//! boundaries: Numbering (this crate's real router and domain, served on a real port with
//! its own database), and REAL processes of Platform Access, the SMS Gateway, Calling,
//! Telephony and Privacy Relay — each with its own database, signed internal HTTP between
//! them, no shared database and no source imports. Customers authenticate with real
//! Platform Access credentials. Providers are simulators (the deterministic SMS provider;
//! SIPp as the carrier on the SIP edge); nothing reaches a carrier.
//!
//! Requires `TEST_DATABASE_URL`, `PLATFORM_ACCESS_BIN` / `PLATFORM_ACCESS_MIGRATION_BIN`
//! and the binaries named by [`Bins`]; without them the journeys are SKIPPED (explicit
//! line) unless `CFMEET_REQUIRE_E2E=1`. The voice journeys also need `asterisk` and
//! `sipp` (the certification SIP edge of infra/telephony/certification), the Telephony /
//! Privacy Relay binaries and `LIVEKIT_SERVER_BIN` ([`VoiceBins`]); without them they are
//! SKIPPED unless `CFMEET_REQUIRE_VOICE_E2E=1` (`make communications-e2e`).
//!
//! These journeys establish software integration, not public-network certification.
mod common;

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use base64::Engine;
use cf_service_auth::{KeyId, ServiceSigningKey};
use chrono::Utc;
use common::{
    acquisition as ax, catalog, kyc as fx, platform_access::PlatformAccess, providers as px,
    strings, Harness, TestDb,
};
use numbering::{
    acquisition::{
        assignments::{AssignmentBasis, AssignmentRequest, TransferRequest},
        provisioning::ProvisionRequest,
        reservations::{customer_actor, ReservationRequest},
        search::{SearchOutcome, SearchRequest},
        Acquisition, AcquisitionSettings,
    },
    domain::{ManualClock, RequestContext},
    numbers::{
        assignments::AssignmentRow,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::SubjectKind,
        routing::RoutingTargets,
        Capability as C, EnvironmentKind, NumberType, OwnerScope, OwnershipClass as O,
        SmsTargetKind as Sms, VoiceTargetKind as V,
    },
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::process::Child;
use uuid::Uuid;

const PROVIDER_SECRET: &str = "e2e-sms-provider-callback-secret-0123456789abcdef";
const REMOTE: &str = "+15550104242";

// ---------------------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------------------

/// The other services' binaries (built with `cargo build --workspace --bins` in each).
struct Bins {
    sms: PathBuf,
    sms_migration: PathBuf,
    calling: PathBuf,
    calling_migration: PathBuf,
}

fn bins() -> Option<Bins> {
    let get = |v: &str| std::env::var_os(v).map(PathBuf::from);
    match (
        get("SMS_GATEWAY_BIN"),
        get("SMS_GATEWAY_MIGRATION_BIN"),
        get("CALLING_BIN"),
        get("CALLING_MIGRATION_BIN"),
    ) {
        (Some(sms), Some(sms_migration), Some(calling), Some(calling_migration)) => Some(Bins {
            sms,
            sms_migration,
            calling,
            calling_migration,
        }),
        _ if std::env::var("CFMEET_REQUIRE_E2E").as_deref() == Ok("1") => {
            panic!("CFMEET_REQUIRE_E2E=1 but the service binaries are not set")
        }
        _ => {
            eprintln!(
                "SKIPPED: cross-service journeys (SMS_GATEWAY_BIN, SMS_GATEWAY_MIGRATION_BIN, \
                 CALLING_BIN, CALLING_MIGRATION_BIN unset)"
            );
            None
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A service database of its own, migrated by the service's own migration binary.
async fn service_db(
    admin: &str,
    prefix: &str,
    migration: &PathBuf,
) -> (String, DatabaseConnection) {
    let name = format!(
        "nb_t_{}_{prefix}{}",
        std::process::id(),
        Uuid::new_v4().simple()
    );
    let conn = Database::connect(admin).await.unwrap();
    conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    conn.close().await.ok();
    let url = common::with_db(admin, &name);
    let out = std::process::Command::new(migration)
        .arg("up")
        .env("DATABASE_URL", &url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} up failed: {}",
        migration.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let db = Database::connect(&url).await.unwrap();
    (url, db)
}

struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

async fn spawn(bin: &PathBuf, env: &[(&str, String)], health: &str) -> Proc {
    spawn_args(bin, &[], env, health).await
}

async fn spawn_args(bin: &PathBuf, args: &[&str], env: &[(&str, String)], health: &str) -> Proc {
    let mut c = tokio::process::Command::new(bin);
    c.args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .kill_on_drop(true);
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c
        .spawn()
        .unwrap_or_else(|e| panic!("{} starts: {e}", bin.display()));
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for _ in 0..600 {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("{} exited during startup: {status}", bin.display());
        }
        if let Ok(r) = http.get(health).send().await {
            if r.status().is_success() {
                return Proc(child);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never became ready ({health})", bin.display());
}

async fn rows(db: &DatabaseConnection, sql: &str) -> Vec<String> {
    db.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .collect()
}

async fn one(db: &DatabaseConnection, sql: &str) -> String {
    rows(db, sql).await.pop().unwrap_or_default()
}

/// Waits until `sql` (one text column) returns `expected`.
async fn eventually(db: &DatabaseConnection, sql: &str, expected: &str) {
    for _ in 0..300 {
        if one(db, sql).await == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "`{sql}` never returned {expected} (last: {})",
        one(db, sql).await
    );
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn varied(seed: u8) -> Vec<u8> {
    (0..32u8)
        .map(|i| i.wrapping_mul(seed).wrapping_add(seed ^ 0x5a))
        .collect()
}

/// The peers' keys: Numbering's harness trusts these exact keys (common::PEER_KEY_PEMS).
fn peer_pems() -> &'static std::collections::HashMap<String, String> {
    common::PEER_KEY_PEMS.get_or_init(|| {
        [
            "calling-test-1",
            "telephony-test-1",
            "sms-gateway-test-1",
            "privacy-relay-test-1",
            "calling-contact-test-1",
        ]
        .into_iter()
        .map(|kid| {
            let k = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
            (kid.to_string(), k.to_pkcs8_pem().to_string())
        })
        .collect()
    })
}

fn public_jwk(kid: &str) -> cf_service_auth::PublicJwk {
    ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &peer_pems()[kid])
        .unwrap()
        .public_jwk()
}

fn bundle(receiver: &str, issuers: &[(&str, &str, &[&str])]) -> String {
    json!({
        "bundle_version": 1,
        "receiver": receiver,
        "runtime_environment": "test",
        "trusted_issuers": issuers.iter().map(|(issuer, kid, perms)| json!({
            "issuer": issuer, "audience": receiver, "runtime_environment": "test",
            "keys": [public_jwk(kid)], "permissions": perms,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

// ---------------------------------------------------------------------------------------
// Numbering seeding (its real domain): an ACTIVE DCN for a real Platform Access customer
// ---------------------------------------------------------------------------------------

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn standing() -> Standing {
    Standing {
        active: true,
        first_party: false,
        live_telecom_allowed: false,
    }
}

fn key() -> String {
    format!("key-{}", Uuid::new_v4().simple())
}

struct Numbers {
    h: Harness,
    a: Acquisition,
    dir: Arc<ax::Principals>,
    runner: numbering::infra::jobs::JobRunner,
    /// Numbering's routing-grant verification key (Privacy Relay's grant trust).
    grant_jwk: cf_service_auth::PublicJwk,
    /// Seconds added to Numbering's grant clock (grant expiry without waiting).
    grant_skew: Arc<std::sync::atomic::AtomicI64>,
}

impl Numbers {
    /// `platform_principals`: owners and routed principals are resolved by the real
    /// Platform Access (the voice journeys, whose principals Calling resolves too);
    /// otherwise by a scripted directory.
    async fn new(pa: Arc<PlatformAccess>, db: Arc<TestDb>, platform_principals: bool) -> Self {
        let h = Harness::build(db, Some(pa), ManualClock::new(Utc::now()));
        let (_, vault) = fx::vault();
        let kyc = fx::service(
            &h,
            vault,
            Arc::new(numbering::numbers::kyc::verifier::DeterministicKycVerifier::new()),
        );
        let h = h.with_kyc(kyc, EligibilityPolicy::default());
        let p = px::setup(&h);
        let h = h.with_providers(p.commands.clone());
        let (h, grant_clock, grant_jwk) = h.with_grants_ttl(300);
        // The peer processes use the system clock: Numbering's clocks follow it.
        let domain_clock = h.clock.clone();
        let grant_skew = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let skew = grant_skew.clone();
        tokio::spawn(async move {
            loop {
                let now = Utc::now();
                domain_clock.set(now);
                grant_clock.set(now.timestamp() + skew.load(std::sync::atomic::Ordering::SeqCst));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        catalog::synthetic_us(&h).await;
        fx::synthetic_us(&h).await;
        let dir = Arc::new(ax::Principals::default());
        let principals: Arc<dyn numbering::identity::PrincipalDirectory> = if platform_principals {
            h.state.principals.clone()
        } else {
            dir.clone()
        };
        let a = ax::of(&h, &p, AcquisitionSettings::default()).with_principals(principals);
        let runner = ax::runner(&h, &p, &a);
        Self {
            h,
            a,
            dir,
            runner,
            grant_jwk,
            grant_skew,
        }
    }

    async fn drain(&self) {
        ax::drain(&self.runner).await;
    }

    async fn id_of(&self, s: &OwnerScope, external_ref: &str) -> Uuid {
        match self
            .a
            .search(
                &ctx(),
                s,
                false,
                &SearchRequest {
                    country: "US".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        {
            SearchOutcome::Page(_) => {}
            SearchOutcome::Throttled { .. } => panic!("throttled"),
        }
        Uuid::parse_str(
            &strings(
                self.h.conn(),
                &format!("SELECT number_resource_id::text FROM number_provider_inventory_refs WHERE external_ref = '{external_ref}'"),
            )
            .await
            .pop()
            .unwrap(),
        )
        .unwrap()
    }

    async fn decide(
        &self,
        s: &OwnerScope,
        subject: (SubjectKind, Uuid),
        class: O,
        number_type: NumberType,
        caps: &[C],
        number: Uuid,
    ) -> Uuid {
        let d = self
            .h
            .numbers
            .evaluate_eligibility(
                &ctx(),
                s,
                standing(),
                &EligibilityPolicy::default(),
                EligibilityRequest {
                    subject_kind: subject.0,
                    subject_ref: subject.1,
                    country: "US".into(),
                    number_type,
                    ownership_class: class,
                    capabilities: caps.to_vec(),
                    number_resource_id: Some(number),
                    provider_id: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(d.outcome, "ELIGIBLE", "{:?}", d.reasons);
        d.id
    }

    /// An ACTIVE assignment of `class` on the synthetic number `external_ref`, every family
    /// of `caps` acknowledged ready by its carrier.
    #[allow(clippy::too_many_arguments)]
    async fn active(
        &self,
        s: &OwnerScope,
        external_ref: &str,
        number_type: NumberType,
        caps: &[C],
        class: O,
        owner: Option<Uuid>,
        routing: RoutingTargets,
    ) -> (Uuid, AssignmentRow) {
        let (id, a) = self
            .prepare(s, external_ref, number_type, caps, class, owner, routing)
            .await;
        self.acknowledge(id, &["VOICE", "SMS"]).await;
        (id, self.activate(s, &a).await)
    }

    /// The assignment of [`Self::active`] with its routes configured, not yet ready.
    #[allow(clippy::too_many_arguments)]
    async fn prepare(
        &self,
        s: &OwnerScope,
        external_ref: &str,
        number_type: NumberType,
        caps: &[C],
        class: O,
        owner: Option<Uuid>,
        routing: RoutingTargets,
    ) -> (Uuid, AssignmentRow) {
        let id = self.id_of(s, external_ref).await;
        let org = (SubjectKind::Organization, s.tenant_id);
        let hold_class = if owner.is_some() {
            O::Organization
        } else {
            class
        };
        let hold = self.decide(s, org, hold_class, number_type, caps, id).await;
        let r = self
            .a
            .create_reservation(
                &ctx(),
                s,
                ReservationRequest {
                    number_resource_id: id,
                    capabilities: caps.to_vec(),
                    hold_seconds: Some(3_600),
                    eligibility_decision_id: Some(hold),
                },
                &key(),
            )
            .await
            .unwrap()
            .reservation;
        self.drain().await;
        self.a
            .provision(
                &ctx(),
                s,
                standing(),
                ProvisionRequest {
                    reservation_id: r.id,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: None,
                },
                &key(),
            )
            .await
            .unwrap();
        self.drain().await;
        let acq = self
            .h
            .numbers
            .acquisition_of_reservation(r.id)
            .await
            .unwrap()
            .unwrap();
        let subject = match owner {
            Some(p) => (SubjectKind::Individual, p),
            None => org,
        };
        let decision = self.decide(s, subject, class, number_type, caps, id).await;
        let a = self
            .a
            .create_assignment(
                &ctx(),
                s,
                standing(),
                AssignmentRequest {
                    basis: AssignmentBasis::Acquisition(acq.id),
                    ownership_class: class,
                    owner_principal_id: owner,
                    targets: routing,
                    capabilities: caps.to_vec(),
                    eligibility_decision_id: Some(decision),
                    pool_id: None,
                },
                &key(),
            )
            .await
            .unwrap()
            .assignment;
        self.a
            .configure_routes(&ctx(), s, acq.id, caps, &key())
            .await
            .unwrap();
        self.drain().await;
        (id, a)
    }

    /// The carriers' readiness of the `families` routes (as the SMS Gateway / Telephony
    /// acknowledge them).
    async fn acknowledge(&self, id: Uuid, families: &[&str]) {
        for r in self.h.numbers.current_routes(id).await.unwrap() {
            if !families.contains(&r.family.as_str()) {
                continue;
            }
            let peer = if r.family == "VOICE" {
                &self.h.peers.telephony
            } else {
                &self.h.peers.sms_gateway
            };
            let reply = self
                .h
                .internal_json(
                    peer,
                    Method::POST,
                    &format!("/internal/v1/number-resources/{id}/route-readiness"),
                    json!({ "family": r.family, "routeRef": r.route_ref, "routeRevision": r.route_revision }),
                )
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
        }
    }

    async fn activate(&self, s: &OwnerScope, a: &AssignmentRow) -> AssignmentRow {
        let a = self
            .a
            .activate_assignment(&ctx(), s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
        assert_eq!(a.status, "ACTIVE");
        a
    }

    async fn routing_revision(&self, id: Uuid) -> i64 {
        strings(
            self.h.conn(),
            &format!(
                "SELECT routing_profile_revision::text FROM dcn_assignments WHERE number_resource_id = '{id}' \
                 AND status = 'ACTIVE'"
            ),
        )
        .await
        .pop()
        .unwrap()
        .parse()
        .unwrap()
    }

    async fn e164(&self, id: Uuid) -> String {
        self.h.numbers.resource(id).await.unwrap().e164
    }
}

fn scope_of(c: &common::platform_access::Customer) -> OwnerScope {
    OwnerScope {
        tenant_id: c.tenant_id,
        application_id: c.application_id,
        environment_id: c.environment_id,
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: c.service_account_id,
    }
}

// ---------------------------------------------------------------------------------------
// The SMS stack
// ---------------------------------------------------------------------------------------

struct SmsStack {
    numbers: Numbers,
    numbering_offline: Arc<std::sync::atomic::AtomicBool>,
    numbering_url: String,
    pa: Arc<PlatformAccess>,
    gateway_url: String,
    gateway_db: DatabaseConnection,
    calling_url: String,
    calling_db: DatabaseConnection,
    http: reqwest::Client,
    _procs: Vec<Proc>,
    _dir: tempdir::Dir,
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

impl SmsStack {
    async fn start(spend_limit_minor: i64) -> Option<Self> {
        let bins = bins()?;
        let db = Arc::new(TestDb::new().await?);
        let admin = db.admin_url().to_string();
        let pa = Arc::new(
            PlatformAccess::start_trusting(
                &admin,
                vec![
                    json!({ "issuer": cf_service_auth::services::CALLING, "keys": [public_jwk("calling-test-1")],
                            "permissions": ["platform-credential.authenticate", "platform-principal.resolve",
                                            "platform-principal.register", "platform-identity.read"] }),
                    json!({ "issuer": cf_service_auth::services::SMS_GATEWAY, "keys": [public_jwk("sms-gateway-test-1")],
                            "permissions": ["platform-credential.authenticate", "platform-principal.resolve"] }),
                ],
            )
            .await?,
        );
        let numbers = Numbers::new(pa.clone(), db, false).await;
        let (numbering_url, numbering_offline) = numbers.h.serve().await;
        let dir = std::env::temp_dir().join(format!("cf-e2e-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, content: &str| {
            let p = dir.join(name);
            std::fs::write(&p, content).unwrap();
            p.display().to_string()
        };

        // Calling: the platform's customer webhook delivery (and, for voice, the Call).
        let (calling_db_url, calling_db) = service_db(&admin, "cl", &bins.calling_migration).await;
        let calling_port = free_port();
        let calling_url = format!("http://127.0.0.1:{calling_port}");
        let calling_env = vec![
            ("DATABASE_URL", calling_db_url),
            ("CALLING_BIND_ADDRESS", format!("127.0.0.1:{calling_port}")),
            ("CALLING_DATABASE_MAX_CONNECTIONS", "5".to_string()),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ("CF_SERVICE_AUTH_KEY_ID", "calling-test-1".to_string()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                file("calling-key.pem", &peer_pems()["calling-test-1"]),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                file(
                    "calling-trust.json",
                    &bundle(
                        cf_service_auth::services::CALLING,
                        &[(
                            cf_service_auth::services::SMS_GATEWAY,
                            "sms-gateway-test-1",
                            &["platform-event.publish"],
                        )],
                    ),
                ),
            ),
            ("CF_PLATFORM_ACCESS_BASE_URL", pa.base_url.clone()),
            ("CF_NUMBERING_BASE_URL", numbering_url.clone()),
            (
                "CALLING_WEBHOOK_SECRET_KEY_ID",
                "webhooks-e2e-1".to_string(),
            ),
            (
                "CALLING_WEBHOOK_SECRET_KEY_FILE",
                file("webhook.key", &b64(&varied(13))),
            ),
            ("CALLING_IDENTITY_SYNC_INTERVAL_MS", "200".to_string()),
            (
                "CALLING_SIGNALING_SIGNING_KEY_FILE",
                file(
                    "signaling.key",
                    "e2e-signaling-signing-key-7f3a91c04d2b58e6a1",
                ),
            ),
            ("RUST_LOG", "calling=warn".to_string()),
        ];
        let calling = spawn(
            &bins.calling,
            &calling_env,
            &format!("{calling_url}/health"),
        )
        .await;

        // The SMS Gateway with the deterministic provider: the first submission times out
        // AFTER the provider created the message (Scenario F).
        let (gateway_db_url, gateway_db) = service_db(&admin, "sg", &bins.sms_migration).await;
        let gateway_port = free_port();
        let gateway_url = format!("http://127.0.0.1:{gateway_port}");
        let gateway_env = vec![
            ("DATABASE_URL", gateway_db_url),
            (
                "SMS_GATEWAY_BIND_ADDRESS",
                format!("127.0.0.1:{gateway_port}"),
            ),
            ("SMS_GATEWAY_DATABASE_MAX_CONNECTIONS", "5".to_string()),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ("CF_SERVICE_AUTH_KEY_ID", "sms-gateway-test-1".to_string()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                file("sms-key.pem", &peer_pems()["sms-gateway-test-1"]),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                file(
                    "sms-trust.json",
                    &bundle(cf_service_auth::services::SMS_GATEWAY, &[]),
                ),
            ),
            ("CF_PLATFORM_ACCESS_BASE_URL", pa.base_url.clone()),
            ("CF_NUMBERING_BASE_URL", numbering_url.clone()),
            ("CF_CALLING_BASE_URL", calling_url.clone()),
            ("SMS_GATEWAY_CONTENT_KEY_ID", "content-e2e-1".to_string()),
            (
                "SMS_GATEWAY_CONTENT_KEY_FILE",
                file("content.key", &b64(&varied(7))),
            ),
            (
                "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
                file("fingerprint.key", &b64(&varied(11))),
            ),
            ("SMS_GATEWAY_PROVIDER", "deterministic".to_string()),
            (
                "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
                file("provider.secret", &b64(PROVIDER_SECRET.as_bytes())),
            ),
            (
                "SMS_GATEWAY_DETERMINISTIC_SCRIPT",
                "ACCEPT,TIMEOUT_AFTER_CREATE".to_string(),
            ),
            ("SMS_GATEWAY_SUBMIT_TIMEOUT_MS", "300".to_string()),
            ("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES", "+1".to_string()),
            (
                "SMS_GATEWAY_DAILY_SPEND_LIMIT_MINOR",
                spend_limit_minor.to_string(),
            ),
            ("SMS_GATEWAY_WORKER_INTERVAL_MS", "100".to_string()),
            ("SMS_GATEWAY_RETRY_BASE_MS", "100".to_string()),
            ("RUST_LOG", "sms_gateway=warn".to_string()),
        ];
        let gateway = spawn(&bins.sms, &gateway_env, &format!("{gateway_url}/ready")).await;
        Some(Self {
            numbers,
            numbering_offline,
            numbering_url,
            pa,
            gateway_url,
            gateway_db,
            calling_url,
            calling_db,
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            _procs: vec![calling, gateway],
            _dir: tempdir::Dir(dir),
        })
    }

    /// A Platform Access customer environment with the SMS and webhook scopes.
    async fn customer(&self) -> common::platform_access::Customer {
        let scopes = ["sms.read", "sms.send", "webhooks.manage", "webhooks.read"];
        self.pa.customer("SANDBOX", &scopes, &scopes).await
    }

    async fn api(
        &self,
        base: &str,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        idempotency: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(method, format!("{base}{path}"))
            .bearer_auth(token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        if let Some(k) = idempotency {
            r = r.header("idempotency-key", k);
        }
        let resp = r.send().await.unwrap();
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn sms(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        self.api(&self.gateway_url, token, method, path, body, key)
            .await
    }

    /// A provider callback signed exactly as the deterministic provider signs.
    async fn provider_callback(
        &self,
        path: &str,
        event_id: &str,
        body: &Value,
        at: i64,
    ) -> (StatusCode, Value) {
        let raw = body.to_string().into_bytes();
        let signature = cf_webhooks::signing::sign(PROVIDER_SECRET.as_bytes(), at, event_id, &raw);
        let resp = self
            .http
            .post(format!(
                "{}/provider-callbacks/v1/sms/deterministic/{path}",
                self.gateway_url
            ))
            .header("content-type", "application/json")
            .header("x-provider-event-id", event_id)
            .header("x-provider-timestamp", at.to_string())
            .header("x-provider-signature", signature)
            .body(raw)
            .send()
            .await
            .unwrap();
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn inbound(&self, event: &str, from: &str, to: &str, text: &str) -> (StatusCode, Value) {
        let now = Utc::now();
        let body = json!({ "kind": "INBOUND", "eventId": event, "from": from, "to": to, "text": text,
                           "receivedAt": now.to_rfc3339() });
        self.provider_callback("inbound", event, &body, now.timestamp())
            .await
    }

    async fn receipt(&self, event: &str, reference: &str, status: &str) -> (StatusCode, Value) {
        let now = Utc::now();
        let body = json!({ "kind": "RECEIPT", "eventId": event, "messageRef": reference, "status": status,
                           "errorCode": null, "costMinor": if status == "DELIVERED" { json!(4) } else { Value::Null },
                           "occurredAt": now.to_rfc3339() });
        self.provider_callback("delivery-receipts", event, &body, now.timestamp())
            .await
    }

    async fn status(&self, token: &str, id: &str) -> Value {
        self.sms(
            token,
            Method::GET,
            &format!("/v1/sms/messages/{id}"),
            None,
            None,
        )
        .await
        .1
    }

    /// Waits until the message reaches `status`.
    async fn until(&self, token: &str, id: &str, status: &str) -> Value {
        for _ in 0..300 {
            let m = self.status(token, id).await;
            if m["status"] == status {
                return m;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!(
            "message {id} never reached {status}: {}",
            self.status(token, id).await
        );
    }
}

fn evt(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

/// Scenarios C, D and the SMS parts of F and G, through Platform Access, Numbering, the
/// SMS Gateway and Calling as separate processes with separate databases.
#[tokio::test]
async fn sms_journeys_across_platform_access_numbering_the_sms_gateway_and_calling() {
    let Some(st) = SmsStack::start(30).await else {
        return;
    };
    let me = st.customer().await;
    let s = scope_of(&me);
    // A team's mobile DCN (SMS in / out) routed to the team.
    let team = Uuid::new_v4();
    let (dcn, assignment) = st
        .numbers
        .active(
            &s,
            "syn-voice-sms-mobile",
            NumberType::Mobile,
            &[C::SmsInbound, C::SmsOutbound],
            O::SharedTeam,
            None,
            RoutingTargets {
                voice: (V::None, None),
                sms: (Sms::SharedTeam, Some(team)),
            },
        )
        .await;
    let e164 = st.numbers.e164(dcn).await;
    // A webhook subscription in Calling for the sms.* events of this environment.
    let (status, hook) = st
        .api(
            &st.calling_url,
            &me.secret,
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": "https://hooks.example.com/cfmeet/sms", "description": "e2e",
                         "eventTypes": ["sms.received", "sms.requested", "sms.submitted", "sms.delivered",
                                        "sms.failed", "sms.unknown", "sms.blocked"] })),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{hook}");
    let hook_id = hook["webhook"]["webhookId"].as_str().unwrap().to_string();

    // ---- Scenario C: inbound SMS → Numbering resolution → sealed storage → inbox / webhook.
    let event = evt("din");
    let (status, ack) = st
        .inbound(&event, REMOTE, &e164, "Is the 3pm slot still free?")
        .await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert_eq!(ack["disposition"], "APPLIED");
    let (status, page) = st
        .sms(
            &me.secret,
            Method::GET,
            "/v1/sms/messages?direction=INBOUND",
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let m = &page["messages"][0];
    assert_eq!(m["content"], "Is the 3pm slot still free?");
    assert_eq!(m["numberResourceId"], json!(dcn));
    assert_eq!(
        m["routing"],
        json!({ "targetKind": "SHARED_TEAM", "targetRef": team })
    );
    assert_eq!(m["remote"]["masked"], "+***4242");
    // The assignment and revision Numbering verified are the message's facts.
    assert_eq!(
        one(
            &st.gateway_db,
            "SELECT assignment_id::text FROM sms_transports WHERE direction = 'INBOUND'"
        )
        .await,
        assignment.id.to_string()
    );
    // The provider's replay of the same callback is a duplicate (one message).
    let (_, again) = st
        .inbound(&event, REMOTE, &e164, "Is the 3pm slot still free?")
        .await;
    assert_eq!(again["status"], "DUPLICATE");
    // A different environment never sees it.
    let stranger = st.customer().await;
    let (_, theirs) = st
        .sms(
            &stranger.secret,
            Method::GET,
            "/v1/sms/messages",
            None,
            None,
        )
        .await;
    assert_eq!(theirs["messages"].as_array().unwrap().len(), 0);
    // Calling received sms.received for THIS environment's subscription (no content).
    eventually(
        &st.calling_db,
        &format!("SELECT count(*)::text FROM webhook_events WHERE event_type = 'sms.received' AND environment_id = '{}'", me.environment_id),
        "1",
    )
    .await;
    let body = one(
        &st.calling_db,
        "SELECT body::text FROM webhook_events WHERE event_type = 'sms.received'",
    )
    .await;
    assert!(
        !body.contains("3pm") && !body.contains("4242") && !body.contains(&e164[1..]),
        "{body}"
    );
    eventually(
        &st.calling_db,
        &format!(
            "SELECT count(*)::text FROM webhook_deliveries WHERE subscription_id = '{hook_id}'"
        ),
        "1",
    )
    .await;

    // ---- Scenario D: outbound SMS → sender grant → policy → provider → receipt → status /
    //      usage / webhook, every service in its own database.
    let k = key();
    let send =
        json!({ "from": { "numberResourceId": dcn }, "to": REMOTE, "text": "Yes, 3pm works." });
    let (status, sent) = st
        .sms(
            &me.secret,
            Method::POST,
            "/v1/sms/messages",
            Some(send.clone()),
            Some(&k),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
    let id = sent["messageId"].as_str().unwrap().to_string();
    // Numbering issued and consumed exactly one SMS_SENDER grant, for THIS message.
    let op = format!("sms:{}", Uuid::parse_str(&id).unwrap().simple());
    assert_eq!(
        strings(
            st.numbers.h.conn(),
            &format!("SELECT count(*)::text FROM number_routing_grants WHERE grant_kind = 'SMS_SENDER' AND operation_ref = '{op}' AND consumed_at IS NOT NULL"),
        )
        .await,
        ["1"]
    );
    let submitted = st.until(&me.secret, &id, "SUBMITTED").await;
    assert_eq!(
        submitted["threadId"], m["threadId"],
        "a reply in the same thread"
    );
    let reference = one(
        &st.gateway_db,
        &format!(
            "SELECT provider_message_ref FROM sms_provider_bindings WHERE transport_id = '{id}'"
        ),
    )
    .await;
    let (status, r) = st.receipt(&evt("dlr"), &reference, "DELIVERED").await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["disposition"], "APPLIED");
    let delivered = st.until(&me.secret, &id, "DELIVERED").await;
    assert_eq!(delivered["cost"]["confirmedMinor"], 4);
    // Replaying the send is the same message; a duplicate receipt changes nothing.
    let (status, replay) = st
        .sms(
            &me.secret,
            Method::POST,
            "/v1/sms/messages",
            Some(send),
            Some(&k),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["messageId"], json!(id));
    assert_eq!(
        one(&st.gateway_db, &format!("SELECT count(*)::text FROM sms_usage_records WHERE transport_id = '{id}' AND status = 'CONFIRMED'")).await,
        "1"
    );
    for t in ["sms.requested", "sms.submitted", "sms.delivered"] {
        eventually(
            &st.calling_db,
            &format!("SELECT count(*)::text FROM webhook_events WHERE event_type = '{t}' AND environment_id = '{}'", me.environment_id),
            "1",
        )
        .await;
    }

    // ---- Scenario F (SMS): a timeout AFTER transmission is UNKNOWN, never resubmitted;
    //      read-back resolves it; a replayed / out-of-order receipt does not regress.
    let (_, sent) = st
        .sms(&me.secret, Method::POST, "/v1/sms/messages",
             Some(json!({ "from": { "numberResourceId": dcn }, "to": "+15550104243", "text": "Reminder: 3pm." })),
             Some(&key()))
        .await;
    let unknown = sent["messageId"].as_str().unwrap().to_string();
    st.until(&me.secret, &unknown, "UNKNOWN").await;
    assert_eq!(
        one(
            &st.gateway_db,
            &format!("SELECT count(*)::text FROM sms_attempts WHERE transport_id = '{unknown}'")
        )
        .await,
        "1",
        "never resubmitted"
    );
    // Read-back after the reconciliation delay finds it at the provider: SUBMITTED.
    st.gateway_db
        .execute_unprepared(&format!(
            "UPDATE sms_transports SET next_attempt_at = now(), version = version + 1 WHERE id = '{unknown}'"
        ))
        .await
        .unwrap();
    st.until(&me.secret, &unknown, "SUBMITTED").await;
    let reference = one(&st.gateway_db, &format!("SELECT provider_message_ref FROM sms_provider_bindings WHERE transport_id = '{unknown}'")).await;
    let delivered_event = evt("dlr");
    st.receipt(&delivered_event, &reference, "DELIVERED").await;
    let (_, dup) = st.receipt(&delivered_event, &reference, "DELIVERED").await;
    assert_eq!(dup["status"], "DUPLICATE");
    let (_, stale) = st.receipt(&evt("dlr"), &reference, "ACCEPTED").await;
    assert_eq!(stale["disposition"], "STALE");
    st.until(&me.secret, &unknown, "DELIVERED").await;
    assert_eq!(
        one(
            &st.gateway_db,
            &format!("SELECT count(*)::text FROM sms_attempts WHERE transport_id = '{unknown}'")
        )
        .await,
        "1",
        "one submission, no duplicate charge"
    );
    // The SMS cost limit (30 minor units): the next estimate exceeds it — blocked, recorded.
    let mut blocked = None;
    for i in 0..6 {
        let (status, r) = st
            .sms(&me.secret, Method::POST, "/v1/sms/messages",
                 Some(json!({ "from": { "numberResourceId": dcn }, "to": format!("+1555010{:04}", 4300 + i), "text": format!("note {i}") })),
                 Some(&key()))
            .await;
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            blocked = Some(r);
            break;
        }
    }
    let blocked = blocked.expect("the spend ceiling is reached");
    assert_eq!(blocked["error"]["reason"], "SPEND_LIMIT_EXCEEDED");
    // A provider callback replayed outside its window / forged is refused.
    let now = Utc::now().timestamp();
    let body = json!({ "kind": "INBOUND", "eventId": "din-old", "from": REMOTE, "to": e164, "text": "old",
                       "receivedAt": Utc::now().to_rfc3339() });
    let (status, _) = st
        .provider_callback("inbound", "din-old", &body, now - 3600)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Numbering unavailable: inbound SMS fails closed (503, nothing stored), sending too.
    st.numbering_offline
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outage = evt("din");
    let (status, _) = st
        .inbound(&outage, REMOTE, &e164, "during the outage")
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // (a fresh environment: the first one has reached its spend ceiling)
    let (status, r) = st
        .sms(
            &stranger.secret,
            Method::POST,
            "/v1/sms/messages",
            Some(json!({ "from": { "numberResourceId": dcn }, "to": "+15550104299", "text": "x" })),
            Some(&key()),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{r}");
    assert_eq!(
        one(&st.gateway_db, "SELECT count(*)::text FROM sms_transports WHERE content_fingerprint IS NOT NULL AND remote_masked = '+***4299'").await,
        "0"
    );
    let _ = (&st.numbering_url, &st.calling_url);
}

/// Scenario G (SMS): suspended, releasing and quarantined DCNs, a historical assignment, a
/// routing update racing delivery, the wrong tenant and an unsupported capability — no
/// delivery to a previous owner, no sender spoofing, no fallback.
#[tokio::test]
async fn sms_lifecycle_safety_across_the_real_service_boundaries() {
    let Some(st) = SmsStack::start(100_000).await else {
        return;
    };
    let me = st.customer().await;
    let s = scope_of(&me);
    let actor = customer_actor(&s);
    let team = Uuid::new_v4();
    let (dcn, a) = st
        .numbers
        .active(
            &s,
            "syn-voice-sms-mobile",
            NumberType::Mobile,
            &[C::SmsInbound, C::SmsOutbound],
            O::SharedTeam,
            None,
            RoutingTargets {
                voice: (V::None, None),
                sms: (Sms::SharedTeam, Some(team)),
            },
        )
        .await;
    let e164 = st.numbers.e164(dcn).await;
    let inbox = |token: String| {
        let st = &st;
        async move {
            st.sms(
                &token,
                Method::GET,
                "/v1/sms/messages?direction=INBOUND&limit=100",
                None,
                None,
            )
            .await
            .1["messages"]
                .as_array()
                .unwrap()
                .clone()
        }
    };
    let send = |token: String, text: &'static str| {
        let st = &st;
        async move {
            st.sms(
                &token,
                Method::POST,
                "/v1/sms/messages",
                Some(json!({ "from": { "numberResourceId": dcn }, "to": REMOTE, "text": text })),
                Some(&key()),
            )
            .await
        }
    };
    assert_eq!(
        st.inbound(&evt("din"), REMOTE, &e164, "before").await.1["disposition"],
        "APPLIED"
    );

    // Wrong tenant: another environment cannot send from this DCN, nor read its messages.
    let stranger = st.customer().await;
    let (status, r) = send(stranger.secret.clone(), "spoof").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["reason"], "SENDER_NOT_AUTHORIZED");
    assert!(inbox(stranger.secret.clone()).await.is_empty());

    // Unsupported capability: a geographic DCN without SMS_OUTBOUND cannot send, and a
    // voice-only DCN receives no SMS (quarantined, never attributed).
    let p = Uuid::new_v4();
    st.numbers
        .dir
        .set(p, numbering::identity::PrincipalStatus::Active);
    let (geo, _) = st
        .numbers
        .active(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound, C::SmsInbound],
            O::EmployeeDirect,
            None,
            RoutingTargets {
                voice: (V::Principal, Some(p)),
                sms: (Sms::Principal, Some(p)),
            },
        )
        .await;
    let (status, r) = st
        .sms(
            &me.secret,
            Method::POST,
            "/v1/sms/messages",
            Some(json!({ "from": { "numberResourceId": geo }, "to": REMOTE, "text": "x" })),
            Some(&key()),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"]["reason"], "SENDER_NOT_AUTHORIZED");
    let (voice_only, _) = st
        .numbers
        .active(
            &s,
            "syn-voice-only",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound],
            O::EmployeeDirect,
            None,
            RoutingTargets {
                voice: (V::Principal, Some(p)),
                sms: (Sms::None, None),
            },
        )
        .await;
    let voice_e164 = st.numbers.e164(voice_only).await;
    assert_eq!(
        st.inbound(&evt("din"), REMOTE, &voice_e164, "to a voice line")
            .await
            .1["disposition"],
        "QUARANTINED"
    );

    // A historical assignment: the number moves to a department (new assignment, new
    // revision). New messages carry the NEW facts and target; the team's routing is gone.
    let dept = Uuid::new_v4();
    let decision = st
        .numbers
        .decide(
            &s,
            (SubjectKind::Organization, s.tenant_id),
            O::Department,
            NumberType::Mobile,
            &[C::SmsInbound, C::SmsOutbound],
            dcn,
        )
        .await;
    let moved = st
        .numbers
        .a
        .transfer_assignment(
            &ctx(),
            &s,
            standing(),
            a.id,
            TransferRequest {
                ownership_class: O::Department,
                owner_principal_id: None,
                targets: RoutingTargets {
                    voice: (V::None, None),
                    sms: (Sms::Department, Some(dept)),
                },
                eligibility_decision_id: Some(decision),
                expected_routing_revision: st.numbers.routing_revision(dcn).await,
            },
            &key(),
        )
        .await
        .unwrap()
        .assignment;
    assert_eq!(
        st.inbound(&evt("din"), REMOTE, &e164, "after the move")
            .await
            .1["disposition"],
        "APPLIED"
    );
    let latest = inbox(me.secret.clone()).await;
    assert_eq!(latest[0]["content"], "after the move");
    assert_eq!(
        latest[0]["routing"],
        json!({ "targetKind": "DEPARTMENT", "targetRef": dept })
    );
    assert_eq!(
        latest[1]["routing"],
        json!({ "targetKind": "SHARED_TEAM", "targetRef": team })
    );
    assert_eq!(
        one(&st.gateway_db, &format!(
            "SELECT count(*)::text FROM sms_transports WHERE assignment_id = '{}' AND target_ref = '{team}'", moved.id
        )).await,
        "0",
        "never the old target under the new assignment"
    );

    // A routing update racing deliveries: every stored message pairs ONE assignment with
    // ITS own target (verified at the final boundary) — never a mix.
    let dept2 = Uuid::new_v4();
    let decision = st
        .numbers
        .decide(
            &s,
            (SubjectKind::Organization, s.tenant_id),
            O::Department,
            NumberType::Mobile,
            &[C::SmsInbound, C::SmsOutbound],
            dcn,
        )
        .await;
    let race = async {
        let mut out = Vec::new();
        for i in 0..6 {
            out.push(
                st.inbound(&evt("din"), REMOTE, &e164, &format!("race {i}"))
                    .await
                    .1["disposition"]
                    .clone(),
            );
        }
        out
    };
    let transfer = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        st.numbers
            .a
            .transfer_assignment(
                &ctx(),
                &s,
                standing(),
                moved.id,
                TransferRequest {
                    ownership_class: O::Department,
                    owner_principal_id: None,
                    targets: RoutingTargets {
                        voice: (V::None, None),
                        sms: (Sms::Department, Some(dept2)),
                    },
                    eligibility_decision_id: Some(decision),
                    expected_routing_revision: st.numbers.routing_revision(dcn).await,
                },
                &key(),
            )
            .await
            .unwrap()
            .assignment
    };
    let (dispositions, third) = tokio::join!(race, transfer);
    assert!(
        dispositions
            .iter()
            .all(|d| d == "APPLIED" || d == "QUARANTINED"),
        "{dispositions:?}"
    );
    // Each (assignment, routing revision) maps to exactly ONE target; the newest
    // revision's messages go to the newest target only.
    assert_eq!(
        one(&st.gateway_db, "SELECT count(*)::text FROM (SELECT assignment_id, routing_revision FROM sms_transports \
             WHERE direction = 'INBOUND' GROUP BY 1, 2 HAVING count(DISTINCT target_ref) > 1) x").await,
        "0"
    );
    assert_eq!(
        one(&st.gateway_db, &format!(
            "SELECT count(*)::text FROM sms_transports WHERE direction = 'INBOUND' AND routing_revision = \
               (SELECT max(routing_revision) FROM sms_transports) AND target_ref <> '{dept2}'"
        )).await,
        "0"
    );
    let _ = third;
    // Suspended: nothing in, nothing out.
    let current = st.numbers.h.numbers.assignment(&s, third.id).await.unwrap();
    let suspended = st
        .numbers
        .h
        .numbers
        .set_assignment_suspended(
            &ctx(),
            actor.clone(),
            current.id,
            current.revision,
            true,
            "CUSTOMER_SUSPENDED",
        )
        .await
        .unwrap();
    assert_eq!(
        st.inbound(&evt("din"), REMOTE, &e164, "while suspended")
            .await
            .1["disposition"],
        "QUARANTINED"
    );
    let (_, r) = send(me.secret.clone(), "while suspended").await;
    assert_eq!(r["error"]["reason"], "SENDER_NOT_AUTHORIZED");
    // Releasing: still nothing.
    let releasing = st
        .numbers
        .h
        .numbers
        .request_release(&ctx(), actor.clone(), suspended.id, suspended.revision)
        .await
        .unwrap();
    assert_eq!(
        st.inbound(&evt("din"), REMOTE, &e164, "while releasing")
            .await
            .1["disposition"],
        "QUARANTINED"
    );
    let (_, r) = send(me.secret.clone(), "while releasing").await;
    assert_eq!(r["error"]["reason"], "SENDER_NOT_AUTHORIZED");
    // Quarantined: the previous owner receives nothing more, ever.
    let b = st
        .numbers
        .h
        .numbers
        .remove_routing(&ctx(), actor.clone(), releasing.id, releasing.revision)
        .await
        .unwrap();
    st.numbers
        .h
        .numbers
        .quarantine_assignment(
            &ctx(),
            actor.clone(),
            b.id,
            b.revision,
            Utc::now() + chrono::Duration::days(30),
        )
        .await
        .unwrap();
    let before = inbox(me.secret.clone()).await.len();
    assert_eq!(
        st.inbound(&evt("din"), REMOTE, &e164, "late content")
            .await
            .1["disposition"],
        "QUARANTINED"
    );
    assert_eq!(
        inbox(me.secret.clone()).await.len(),
        before,
        "no previous-owner delivery"
    );
    assert!(inbox(stranger.secret.clone()).await.is_empty());
    // Quarantined content is sealed at rest and attributed to no customer.
    assert_eq!(
        one(
            &st.gateway_db,
            "SELECT count(*)::text FROM sms_quarantined_inbound WHERE content_sealed LIKE 'v2.%'"
        )
        .await,
        one(
            &st.gateway_db,
            "SELECT count(*)::text FROM sms_quarantined_inbound"
        )
        .await
    );
}

// ---------------------------------------------------------------------------------------
// The voice stack: the real SIP edge (Asterisk, infra/telephony/certification) with SIPp
// as the carrier, and the real Telephony, Privacy Relay and Calling processes (Calling's
// application media on a real LiveKit server)
// ---------------------------------------------------------------------------------------

/// The certification edge's fixed loopback topology (pjsip.conf / ari.conf / http.conf).
const ARI_URL: &str = "http://127.0.0.1:8088";
const ARI_USER: &str = "cfmeet-cert";
const ARI_APP: &str = "cfmeet-cert";
const EDGE: &str = "127.0.0.1:5070";
/// Simulated carrier A (pjsip endpoint `peer-ng-a`, identified by source address).
const CARRIER: &str = "127.0.0.2";
/// The edge's application-media peer (pjsip endpoint `app-media`): LiveKit SIP.
const APP_MEDIA: &str = "127.0.0.3";

/// A LiveKit API access token (HS256) with `grants`.
fn livekit_token(key: &str, secret: &str, grants: Value) -> String {
    use sha2::{Digest, Sha256};
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let now = Utc::now().timestamp();
    let mut claims = json!({ "iss": key, "nbf": now - 5, "exp": now + 600 });
    for (k, v) in grants.as_object().unwrap() {
        claims[k] = v.clone();
    }
    let input = format!(
        "{}.{}",
        b64(br#"{"alg":"HS256","typ":"JWT"}"#),
        b64(claims.to_string().as_bytes())
    );
    // HMAC-SHA256 (RFC 2104) over the signing input.
    let mut k = secret.as_bytes().to_vec();
    if k.len() > 64 {
        k = Sha256::digest(&k).to_vec();
    }
    k.resize(64, 0);
    let pad = |byte: u8| k.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::digest([pad(0x36), input.as_bytes().to_vec()].concat());
    let mac = Sha256::digest([pad(0x5c), inner.to_vec()].concat());
    format!("{input}.{}", b64(&mac))
}

struct VoiceBins {
    calling: PathBuf,
    calling_migration: PathBuf,
    calling_admin: PathBuf,
    telephony: PathBuf,
    telephony_migration: PathBuf,
    telephony_admin: PathBuf,
    relay: PathBuf,
    relay_migration: PathBuf,
    relay_admin: PathBuf,
    livekit: PathBuf,
    /// LiveKit SIP: Calling's media gateway dials the edge's application-media peer.
    livekit_sip: PathBuf,
    /// Redis for LiveKit's SIP service.
    redis: PathBuf,
    /// A Python with the LiveKit RTC SDK: the accepting application's real media client
    /// (Calling's `tests/rtc/rtc_client.py`).
    rtc_python: PathBuf,
}

fn tool_available(binary: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {binary}")])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The voice journeys need the SIP edge and SIPp, LiveKit and five service binaries;
/// SKIPPED without them unless `CFMEET_REQUIRE_VOICE_E2E=1`.
fn voice_bins() -> Option<VoiceBins> {
    let get = |v: &str| std::env::var_os(v).map(PathBuf::from);
    let next_to = |bin: &Option<PathBuf>, name: &str| bin.as_ref().map(|b| b.with_file_name(name));
    let calling = get("CALLING_BIN");
    let telephony = get("TELEPHONY_BIN");
    let relay = get("PRIVACY_RELAY_BIN");
    let found = (|| {
        Some(VoiceBins {
            calling_admin: get("CALLING_ADMIN_BIN")
                .or_else(|| next_to(&calling, "platform_admin"))?,
            telephony_admin: next_to(&telephony, "telephony-admin")?,
            relay_admin: next_to(&relay, "privacy-relay-admin")?,
            calling: calling.clone()?,
            calling_migration: get("CALLING_MIGRATION_BIN")?,
            telephony: telephony.clone()?,
            telephony_migration: get("TELEPHONY_MIGRATION_BIN")?,
            relay: relay.clone()?,
            relay_migration: get("PRIVACY_RELAY_MIGRATION_BIN")?,
            livekit: get("LIVEKIT_SERVER_BIN")?,
            livekit_sip: get("LIVEKIT_SIP_BIN")?,
            redis: get("REDIS_SERVER_BIN").or_else(|| {
                tool_available("redis-server").then(|| PathBuf::from("redis-server"))
            })?,
            rtc_python: get("CALLING_RTC_CERT_PYTHON")?,
        })
    })()
    .filter(|_| tool_available("asterisk") && tool_available("sipp"));
    if found.is_none() {
        assert!(
            std::env::var("CFMEET_REQUIRE_VOICE_E2E").as_deref() != Ok("1"),
            "CFMEET_REQUIRE_VOICE_E2E=1 but asterisk / sipp or a voice binary is missing"
        );
        eprintln!(
            "SKIPPED: voice journeys (asterisk, sipp, redis-server, LIVEKIT_SERVER_BIN, \
             LIVEKIT_SIP_BIN, CALLING_RTC_CERT_PYTHON, CALLING_BIN, TELEPHONY_BIN, PRIVACY_RELAY_BIN and their \
             migration binaries are required)"
        );
    }
    found
}

fn cert_dir() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../infra/telephony/certification")
        .canonicalize()
        .unwrap()
}

/// The certification edge's test-only ARI credential (infra/telephony/certification/ari.conf).
fn ari_password() -> String {
    std::fs::read_to_string(cert_dir().join("ari.conf"))
        .unwrap()
        .lines()
        .find_map(|l| l.trim().strip_prefix("password = ").map(str::to_string))
        .expect("ari.conf names the certification password")
}

/// Runs an operator CLI; panics with its stderr on failure.
fn run_cli(bin: &PathBuf, args: &[&str], env: &[(&str, String)]) -> String {
    let mut c = std::process::Command::new(bin);
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c
        .output()
        .unwrap_or_else(|e| panic!("{} runs: {e}", bin.display()));
    assert!(
        out.status.success(),
        "{} {args:?}: {}{}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

struct Asterisk {
    child: std::process::Child,
    run: PathBuf,
}

impl Drop for Asterisk {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.run);
    }
}

impl Asterisk {
    /// The certification edge configuration on loopback (`@RUNDIR@` = a private run dir).
    fn start() -> Self {
        let run = std::env::temp_dir().join(format!("cf-e2e-edge-{}", Uuid::new_v4().simple()));
        for d in ["etc", "varlib", "spool", "run", "log", "agi", "moh"] {
            std::fs::create_dir_all(run.join(d)).unwrap();
        }
        std::fs::write(run.join("moh/hold-tone.sln"), vec![0u8; 16_000]).unwrap();
        for entry in std::fs::read_dir(cert_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "conf") {
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("@RUNDIR@", run.to_str().unwrap());
                std::fs::write(run.join("etc").join(path.file_name().unwrap()), text).unwrap();
            }
        }
        let log = std::fs::File::create(run.join("console.log")).unwrap();
        let child = std::process::Command::new("asterisk")
            .args([
                "-C",
                run.join("etc/asterisk.conf").to_str().unwrap(),
                "-f",
                "-n",
            ])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("asterisk starts");
        Self { child, run }
    }

    fn console(&self) -> String {
        let text = std::fs::read_to_string(self.run.join("console.log")).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

/// The application's real WebRTC media client (Calling's RTC certification client), driven
/// one JSON line at a time: it joins LiveKit through Calling's media gateway with a
/// Calling-issued credential, exactly as an application endpoint does.
struct RtcApp {
    _child: Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

impl RtcApp {
    fn spawn(python: &PathBuf) -> Self {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../calling/tests/rtc/rtc_client.py");
        let mut child = tokio::process::Command::new(python)
            .arg("-I")
            .arg(script)
            .env_remove("HTTPS_PROXY")
            .env_remove("HTTP_PROXY")
            .env_remove("https_proxy")
            .env_remove("http_proxy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("the RTC client starts");
        let stdin = child.stdin.take().unwrap();
        let lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(
            child.stdout.take().unwrap(),
        ));
        Self {
            _child: child,
            stdin,
            lines,
        }
    }

    async fn cmd(&mut self, v: Value) -> Value {
        use tokio::io::AsyncWriteExt;
        self.stdin
            .write_all(format!("{v}\n").as_bytes())
            .await
            .unwrap();
        self.stdin.flush().await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(60), self.lines.next_line())
            .await
            .expect("the RTC client answers")
            .unwrap()
            .expect("the RTC client is alive");
        serde_json::from_str(&line).unwrap()
    }
}

/// One SIPp carrier call from carrier A to `called`.
struct Sipp {
    child: std::process::Child,
    log_dir: PathBuf,
}

impl Drop for Sipp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.log_dir);
    }
}

impl Sipp {
    /// Carrier A calling `called` from `caller` (UAC, to the edge).
    fn call(scenario: &str, called: &str, caller: &str) -> Self {
        Self::run(
            scenario,
            Some(EDGE),
            "5081",
            &[("called", called), ("caller", caller)],
        )
    }

    /// Carrier A answering calls the edge sends it (UAS on its trunk address).
    fn serve(scenario: &str) -> Self {
        Self::run(scenario, None, "5080", &[])
    }

    fn run(scenario: &str, remote: Option<&str>, port: &str, keys: &[(&str, &str)]) -> Self {
        let log_dir = std::env::temp_dir().join(format!("cf-e2e-sipp-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&log_dir).unwrap();
        let mut command = std::process::Command::new("sipp");
        if let Some(remote) = remote {
            command.arg(remote);
        }
        for (k, v) in keys {
            command.args(["-key", k, v]);
        }
        let child = command
            .args([
                "-sf",
                cert_dir().join("sipp").join(scenario).to_str().unwrap(),
                "-i",
                CARRIER,
                "-p",
                port,
                "-mi",
                CARRIER,
                "-min_rtp_port",
                "16400",
                "-max_rtp_port",
                "16498",
                "-m",
                "1",
                "-l",
                "1",
                "-timeout",
                "60s",
                "-timeout_error",
                "-nostdin",
                "-trace_err",
                "-trace_msg",
            ])
            .current_dir(&log_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(log_dir.join("stderr.log")).unwrap())
            .spawn()
            .expect("sipp starts");
        Self { child, log_dir }
    }

    /// Waits for the scenario's end; true = SIPp reported success.
    async fn finished_ok(&mut self) -> bool {
        for _ in 0..700 {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.success();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Every SIP message SIPp exchanged (its `-trace_msg` log).
    fn messages(&self) -> String {
        std::fs::read_dir(&self.log_dir)
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct VoiceStack {
    numbers: Numbers,
    numbering_offline: Arc<std::sync::atomic::AtomicBool>,
    pa: Arc<PlatformAccess>,
    bins: VoiceBins,
    calling_url: String,
    calling_metrics: String,
    calling_db: DatabaseConnection,
    calling_db_url: String,
    telephony_db: DatabaseConnection,
    telephony_admin_env: Vec<(&'static str, String)>,
    relay_db: DatabaseConnection,
    relay_url: String,
    http: reqwest::Client,
    asterisk: Asterisk,
    _procs: Vec<Proc>,
    _dir: tempdir::Dir,
}

impl VoiceStack {
    async fn start() -> Option<Self> {
        let bins = voice_bins()?;
        let db = Arc::new(TestDb::new().await?);
        let admin = db.admin_url().to_string();
        let pa = Arc::new(
            PlatformAccess::start_trusting(
                &admin,
                vec![json!({ "issuer": cf_service_auth::services::CALLING, "keys": [public_jwk("calling-test-1")],
                             "permissions": ["platform-credential.authenticate", "platform-principal.resolve",
                                             "platform-principal.register", "platform-identity.read"] })],
            )
            .await?,
        );
        let numbers = Numbers::new(pa.clone(), db, true).await;
        let (numbering_url, numbering_offline) = numbers.h.serve().await;
        let dir = std::env::temp_dir().join(format!("cf-e2e-voice-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("provider-secrets")).unwrap();
        let file = |name: &str, content: &str| {
            let p = dir.join(name);
            std::fs::write(&p, content).unwrap();
            p.display().to_string()
        };
        let (calling_port, calling_metrics_port, relay_port, telephony_port) =
            (free_port(), free_port(), free_port(), free_port());
        let calling_url = format!("http://127.0.0.1:{calling_port}");
        let relay_url = format!("http://127.0.0.1:{relay_port}");
        let telephony_url = format!("http://127.0.0.1:{telephony_port}");
        let mut procs = Vec::new();

        // Redis, LiveKit (Calling's application media; its webhooks reach Calling) and LiveKit
        // SIP (Calling's public-bridge media gateway), loopback only, test-only key pair.
        let redis_port = free_port();
        let redis = tokio::process::Command::new(&bins.redis)
            .args([
                "--port",
                &redis_port.to_string(),
                "--bind",
                "127.0.0.1",
                "--save",
                "",
                "--appendonly",
                "no",
            ])
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("redis starts");
        procs.push(Proc(redis));
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", redis_port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (lk_port, lk_tcp) = (free_port(), free_port());
        let lk_secret = format!("e2e-livekit-{}", Uuid::new_v4().simple());
        let lk_config = file(
            "livekit.yaml",
            &format!(
                "port: {lk_port}\nbind_addresses: [\"127.0.0.1\"]\nrtc:\n  tcp_port: {lk_tcp}\n  \
                 port_range_start: 51000\n  port_range_end: 51100\n  use_external_ip: false\n  \
                 node_ip: 127.0.0.1\nredis:\n  address: 127.0.0.1:{redis_port}\n\
                 keys:\n  cfmeet-e2e: {lk_secret}\nwebhook:\n  api_key: cfmeet-e2e\n  urls:\n    \
                 - {calling_url}/internal/provider/media/events\nroom:\n  auto_create: false\n\
                 logging:\n  level: warn\n"
            ),
        );
        procs.push(
            spawn_args(
                &bins.livekit,
                &["--config", &lk_config],
                &[("HOME", dir.display().to_string())],
                &format!("http://127.0.0.1:{lk_port}/"),
            )
            .await,
        );
        // LiveKit SIP sends from the edge's application-media peer address (pjsip.conf).
        let sip_health = free_port();
        let sip_config = file(
            "livekit-sip.yaml",
            &format!(
                "api_key: cfmeet-e2e\napi_secret: {lk_secret}\nws_url: ws://127.0.0.1:{lk_port}\n\
                 redis:\n  address: 127.0.0.1:{redis_port}\nsip_port: 5060\nlisten_ip: {APP_MEDIA}\n\
                 nat_1_to_1_ip: {APP_MEDIA}\nuse_external_ip: false\nrtp_port: 52000-52200\n\
                 health_port: {sip_health}\nlogging:\n  level: warn\n"
            ),
        );
        procs.push(
            spawn_args(
                &bins.livekit_sip,
                &["--config", &sip_config],
                &[("HOME", dir.display().to_string())],
                &format!("http://127.0.0.1:{sip_health}/"),
            )
            .await,
        );
        // The outbound trunk LiveKit SIP dials: the edge's application-media entry.
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let trunk: Value = http
            .post(format!("http://127.0.0.1:{lk_port}/twirp/livekit.SIP/CreateSIPOutboundTrunk"))
            .bearer_auth(livekit_token("cfmeet-e2e", &lk_secret, json!({ "sip": { "admin": true } })))
            .json(&json!({ "trunk": { "name": "cfmeet-edge", "address": EDGE, "numbers": ["cfmeet-edge"], "transport": 1 } }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let trunk_id = trunk["sip_trunk_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no trunk: {trunk}"))
            .to_string();

        // The SIP edge, and its ARI.
        let asterisk = Asterisk::start();
        let password = ari_password();
        let mut up = false;
        for _ in 0..300 {
            if let Ok(r) = http
                .get(format!("{ARI_URL}/ari/asterisk/info"))
                .basic_auth(ARI_USER, Some(&password))
                .send()
                .await
            {
                if r.status().is_success() {
                    up = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(up, "the SIP edge did not come up:\n{}", asterisk.console());

        // Privacy Relay: the Contact Vault (caller protection) and Numbering's grant trust.
        let (relay_db_url, relay_db) = service_db(&admin, "pr", &bins.relay_migration).await;
        for (purpose, version, name) in [
            ("encryption", "ck-e2e-1", "contact-keyring.json"),
            ("fingerprint", "fk-e2e-1", "fingerprint-keyring.json"),
        ] {
            let out = dir.join(name).display().to_string();
            run_cli(
                &bins.relay_admin,
                &[
                    "contact-vault",
                    "generate-keyring",
                    "--purpose",
                    purpose,
                    "--version",
                    version,
                    "--out",
                    &out,
                ],
                &[],
            );
        }
        let relay_env = vec![
            ("DATABASE_URL", relay_db_url),
            ("PRIVACY_RELAY_BIND_ADDRESS", format!("127.0.0.1:{relay_port}")),
            ("PRIVACY_RELAY_ENABLED", "true".to_string()),
            ("RELAY_NUMBER_POOL_ENABLED", "true".to_string()),
            ("PRIVACY_RELAY_WORKER_INTERVAL_MS", "200".to_string()),
            ("CONTACT_VAULT_ENABLED", "true".to_string()),
            ("CONTACT_VAULT_KEYRING_FILE", dir.join("contact-keyring.json").display().to_string()),
            ("CONTACT_FINGERPRINT_KEYRING_FILE", dir.join("fingerprint-keyring.json").display().to_string()),
            (
                "CONTACT_DELEGATION_TRUST_BUNDLE_FILE",
                file(
                    "contact-trust.json",
                    &json!({
                        "bundle_version": 1, "token_use": "CONTACT_VAULT_ACCESS",
                        "audience": "cfmeet.privacy-relay", "runtime_environment": "test",
                        "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": [public_jwk("calling-contact-test-1")],
                            "scopes": ["privacy.contacts.read", "privacy.contacts.write", "privacy.contacts.verify"] }],
                    })
                    .to_string(),
                ),
            ),
            (
                "NUMBER_GRANT_TRUST_BUNDLE_FILE",
                file(
                    "number-grant-trust.json",
                    &json!({
                        "bundle_version": 1, "token_use": "NUMBER_ROUTING_GRANT",
                        "audience": "cfmeet.privacy-relay", "runtime_environment": "test",
                        "trusted_issuers": [{ "issuer": "cfmeet.numbering", "keys": [numbers.grant_jwk.clone()],
                            "grant_kinds": ["INBOUND_VOICE"] }],
                    })
                    .to_string(),
                ),
            ),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-test-1".to_string()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                file("relay-key.pem", &peer_pems()["privacy-relay-test-1"]),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                file(
                    "relay-trust.json",
                    &bundle(
                        cf_service_auth::services::PRIVACY_RELAY,
                        &[
                            (
                                cf_service_auth::services::CALLING,
                                "calling-test-1",
                                &[
                                    "communication.authorize",
                                    "communication-context.read",
                                    "communication-context.write",
                                    "relay-session.read",
                                    "relay-session.write",
                                    "protected-contact.grant-for-execution",
                                    "relay-number.assign",
                                ],
                            ),
                            (
                                cf_service_auth::services::TELEPHONY,
                                "telephony-test-1",
                                &[
                                    "protected-contact.resolve-for-execution",
                                    "relay-number.resolve-inbound",
                                    "protected-contact.protect-inbound",
                                ],
                            ),
                        ],
                    ),
                ),
            ),
            ("CALLING_INTERNAL_BASE_URL", calling_url.clone()),
            ("CF_INTERNAL_HTTP_TIMEOUT_MS", "2000".to_string()),
            ("RUST_LOG", "privacy_relay=warn".to_string()),
        ];
        procs.push(spawn(&bins.relay, &relay_env, &format!("{relay_url}/health")).await);

        // Telephony: carrier A as a SIP trunk on the edge (the route serving the DCNs).
        let (telephony_db_url, telephony_db) =
            service_db(&admin, "tl", &bins.telephony_migration).await;
        std::fs::write(
            dir.join("provider-secrets").join("sip-a.secret"),
            "e2e-sip-trunk-credential-reference-only",
        )
        .unwrap();
        let telephony_auth = vec![
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ("CF_SERVICE_AUTH_KEY_ID", "telephony-test-1".to_string()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                file("telephony-key.pem", &peer_pems()["telephony-test-1"]),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                file(
                    "telephony-trust.json",
                    &bundle(
                        cf_service_auth::services::TELEPHONY,
                        &[(
                            cf_service_auth::services::CALLING,
                            "calling-test-1",
                            &[
                                "telephony-leg.originate",
                                "telephony-leg.terminate",
                                "telephony-leg.hold",
                                "telephony-leg.resume",
                                "telephony-bridge.manage",
                                "telephony-status.read",
                            ],
                        )],
                    ),
                ),
            ),
            ("NUMBERING_INTERNAL_BASE_URL", numbering_url.clone()),
            ("CALLING_INTERNAL_BASE_URL", calling_url.clone()),
            ("PRIVACY_RELAY_INTERNAL_BASE_URL", relay_url.clone()),
        ];
        let mut telephony_admin_env = telephony_auth.clone();
        telephony_admin_env.push(("DATABASE_URL", telephony_db_url.clone()));
        telephony_admin_env.push(("CF_OPERATOR_ID", "ops.e2e".to_string()));
        for args in [
            vec![
                "provider",
                "add",
                "--id",
                "sip-a",
                "--adapter",
                "SIP_TRUNK",
                "--capabilities",
                "OUTBOUND_VOICE,INBOUND_VOICE,STATUS_QUERY,EVENTS,HEALTH,CAPACITY",
                "--credential-ref",
                "sip-a.secret",
            ],
            vec!["provider", "enable", "sip-a"],
            vec![
                "sip-peer",
                "add",
                "--id",
                "peer-ng-a",
                "--provider",
                "sip-a",
                "--secret-ref",
                "trunk/ng-a",
                "--allowed-cidrs",
                "127.0.0.2/32",
            ],
            vec![
                "route",
                "add",
                "--id",
                "ng-a",
                "--provider",
                "sip-a",
                "--countries",
                "NG,US",
                "--capabilities",
                "VOICE_OUTBOUND,VOICE_INBOUND",
                "--priority",
                "10",
                "--sip-peer",
                "peer-ng-a",
                "--estimated-leg-cost-minor",
                "0",
                "--cost-currency",
                "USD",
                "--interconnect-class",
                "SYNTHETIC_TEST",
            ],
            vec!["route", "enable", "ng-a"],
        ] {
            run_cli(&bins.telephony_admin, &args, &telephony_admin_env);
        }
        let mut telephony_env = telephony_auth;
        telephony_env.extend([
            ("DATABASE_URL", telephony_db_url),
            (
                "TELEPHONY_BIND_ADDRESS",
                format!("127.0.0.1:{telephony_port}"),
            ),
            ("TELEPHONY_ENABLED", "true".to_string()),
            ("PUBLIC_OUTBOUND_CALLING_ENABLED", "true".to_string()),
            ("PUBLIC_INBOUND_CALLING_ENABLED", "true".to_string()),
            ("TELEPHONY_WORKER_INTERVAL_MS", "100".to_string()),
            (
                "TELECOM_PROVIDER_SECRETS_DIR",
                dir.join("provider-secrets").display().to_string(),
            ),
            ("TELEPHONY_SIP_EDGE", "asterisk-ari".to_string()),
            ("TELEPHONY_ARI_URL", ARI_URL.to_string()),
            ("TELEPHONY_ARI_USERNAME", ARI_USER.to_string()),
            (
                "TELEPHONY_ARI_PASSWORD_FILE",
                file("ari.password", &password),
            ),
            ("TELEPHONY_ARI_APP", ARI_APP.to_string()),
            (
                "TELEPHONY_SYNTHETIC_INTERCONNECT_ENABLED",
                "true".to_string(),
            ),
            ("CF_INTERNAL_HTTP_TIMEOUT_MS", "2000".to_string()),
            ("RUST_LOG", "telephony=warn".to_string()),
        ]);
        procs.push(
            spawn(
                &bins.telephony,
                &telephony_env,
                &format!("{telephony_url}/health"),
            )
            .await,
        );
        // Telephony's control application is registered on the edge (event WebSocket up).
        let mut registered = false;
        for _ in 0..300 {
            if let Ok(r) = http
                .get(format!("{ARI_URL}/ari/applications/{ARI_APP}"))
                .basic_auth(ARI_USER, Some(&password))
                .send()
                .await
            {
                if r.status().is_success() {
                    registered = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            registered,
            "Telephony never registered on the edge:\n{}",
            asterisk.console()
        );

        // Calling: the Call authority (Numbering grant verification, routing, endpoints).
        let (calling_db_url, calling_db) = service_db(&admin, "cl", &bins.calling_migration).await;
        let calling_env = vec![
            ("DATABASE_URL", calling_db_url.clone()),
            ("CALLING_BIND_ADDRESS", format!("127.0.0.1:{calling_port}")),
            (
                "CALLING_METRICS_ADDRESS",
                format!("127.0.0.1:{calling_metrics_port}"),
            ),
            ("CALLING_DATABASE_MAX_CONNECTIONS", "8".to_string()),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ("CF_SERVICE_AUTH_KEY_ID", "calling-test-1".to_string()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                file("calling-key.pem", &peer_pems()["calling-test-1"]),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                file(
                    "calling-trust.json",
                    &bundle(
                        cf_service_auth::services::CALLING,
                        &[
                            (
                                cf_service_auth::services::PRIVACY_RELAY,
                                "privacy-relay-test-1",
                                &["communication-authority.enforce"],
                            ),
                            (
                                cf_service_auth::services::TELEPHONY,
                                "telephony-test-1",
                                &[
                                    "telephony-event.apply",
                                    "telephony-reconciliation.apply",
                                    "telephony-inbound.offer",
                                ],
                            ),
                        ],
                    ),
                ),
            ),
            ("CF_PLATFORM_ACCESS_BASE_URL", pa.base_url.clone()),
            ("CF_NUMBERING_BASE_URL", numbering_url.clone()),
            ("CF_PRIVACY_RELAY_BASE_URL", relay_url.clone()),
            ("CF_TELEPHONY_BASE_URL", telephony_url.clone()),
            ("CALLING_PUBLIC_INBOUND_ENABLED", "true".to_string()),
            ("CALLING_PUBLIC_OUTBOUND_ENABLED", "true".to_string()),
            ("CALLING_SYNTHETIC_INTERCONNECT_ENABLED", "true".to_string()),
            ("CALLING_LIVEKIT_SIP_TRUNK_ID", trunk_id),
            (
                "CONTACT_DELEGATION_KEY_ID",
                "calling-contact-test-1".to_string(),
            ),
            (
                "CONTACT_DELEGATION_PRIVATE_KEY_FILE",
                file("contact-key.pem", &peer_pems()["calling-contact-test-1"]),
            ),
            ("CALLING_MEDIA_PROVIDER", "livekit".to_string()),
            ("CALLING_LIVEKIT_URL", format!("ws://127.0.0.1:{lk_port}")),
            (
                "CALLING_LIVEKIT_API_URL",
                format!("http://127.0.0.1:{lk_port}"),
            ),
            ("CALLING_LIVEKIT_API_KEY", "cfmeet-e2e".to_string()),
            (
                "CALLING_LIVEKIT_API_SECRET_FILE",
                file("livekit.secret", &lk_secret),
            ),
            (
                "CALLING_MEDIA_GATEWAY_URL",
                format!("ws://127.0.0.1:{calling_port}/v1/calling/media"),
            ),
            (
                "CALLING_MEDIA_EDGE_SIGNING_KEY_FILE",
                file(
                    "media-edge.key",
                    &format!("e2e-media-edge-{}", Uuid::new_v4().simple()),
                ),
            ),
            (
                "CALLING_WEBHOOK_SECRET_KEY_ID",
                "webhooks-e2e-1".to_string(),
            ),
            (
                "CALLING_WEBHOOK_SECRET_KEY_FILE",
                file("webhook.key", &b64(&varied(13))),
            ),
            ("CALLING_IDENTITY_SYNC_INTERVAL_MS", "200".to_string()),
            (
                "CALLING_SIGNALING_SIGNING_KEY_FILE",
                file(
                    "signaling.key",
                    "e2e-signaling-signing-key-7f3a91c04d2b58e6a1",
                ),
            ),
            ("RUST_LOG", "calling=warn".to_string()),
        ];
        procs.push(
            spawn(
                &bins.calling,
                &calling_env,
                &format!("{calling_url}/health"),
            )
            .await,
        );
        Some(Self {
            numbers,
            numbering_offline,
            pa,
            bins,
            calling_url,
            calling_metrics: format!("http://127.0.0.1:{calling_metrics_port}/metrics"),
            calling_db,
            calling_db_url,
            telephony_db,
            telephony_admin_env,
            relay_db,
            relay_url,
            http,
            asterisk,
            _procs: procs,
            _dir: tempdir::Dir(dir),
        })
    }
}

/// The Calling metric `series` (`name{labels}`), 0 when not yet recorded.
fn metric_value(text: &str, series: &str) -> f64 {
    text.lines()
        .find(|l| l.starts_with(series) && l[series.len()..].starts_with(' '))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// Everything a database stores, as text (every table of its public schema).
async fn dump(db: &DatabaseConnection) -> String {
    let mut out = String::new();
    for table in rows(
        db,
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public'",
    )
    .await
    {
        for row in rows(
            db,
            &format!("SELECT row_to_json(x)::text FROM \"{table}\" x"),
        )
        .await
        {
            out.push_str(&row);
            out.push('\n');
        }
    }
    out
}

/// The offer Telephony makes Calling for a resolution Numbering answered (references and
/// Numbering's grant only — never a number).
fn offer_of(resolution: &Value) -> Value {
    json!({
        "schemaVersion": "1",
        "telephonyLegId": Uuid::new_v4(),
        "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
        "scope": resolution["scope"],
        "dcnResolution": {
            "numberResourceId": resolution["numberResourceId"],
            "assignmentId": resolution["assignmentId"],
            "routingRevision": resolution["routingRevision"],
            "ownershipClass": resolution["ownershipClass"],
            "target": resolution["target"],
            "grant": resolution["grant"]["grant"],
        },
        "callerContactReference": Uuid::new_v4(),
        "maxDurationSeconds": 3600,
    })
}

impl VoiceStack {
    /// A Platform Access customer environment allowed to manage principals and calls.
    async fn customer(&self) -> common::platform_access::Customer {
        let scopes = [
            "principals.read",
            "principals.write",
            "calls.read",
            "calls.write",
            "privacy.contexts.read",
            "privacy.contexts.write",
            "privacy.sessions.read",
            "privacy.sessions.write",
            "privacy.contacts.read",
            "privacy.contacts.write",
            "privacy.contacts.verify",
        ];
        self.pa.customer("SANDBOX", &scopes, &scopes).await
    }

    async fn api(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(method.clone(), format!("{}{path}", self.calling_url))
            .bearer_auth(token);
        if method == Method::POST {
            r = r.header("idempotency-key", key());
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    /// Calling's operator configuration of the environment (Part E): a routing policy
    /// allowing application, phone-to-app and app-to-phone routes, a spend policy and the
    /// credential inbound public calls are recorded against.
    async fn configure_calling(&self, c: &common::platform_access::Customer) {
        eventually(
            &self.calling_db,
            &format!(
                "SELECT count(*)::text FROM platform_environments WHERE id = '{}'",
                c.environment_id
            ),
            "1",
        )
        .await;
        let tenant = one(
            &self.calling_db,
            &format!(
                "SELECT slug::text FROM platform_tenants WHERE id = '{}'",
                c.tenant_id
            ),
        )
        .await;
        let app = one(
            &self.calling_db,
            &format!(
                "SELECT slug::text FROM platform_applications WHERE id = '{}'",
                c.application_id
            ),
        )
        .await;
        let env = [
            ("DATABASE_URL", self.calling_db_url.clone()),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ("CF_OPERATOR_ID", "ops.e2e".to_string()),
        ];
        let scope = [
            "--tenant",
            &tenant,
            "--application",
            &app,
            "--environment",
            "sandbox",
        ];
        let policy = self
            ._dir
            .0
            .join(format!("routing-{}.json", c.environment_id));
        std::fs::write(
            &policy,
            json!({
                "allowedRouteClasses": ["APP_RTC", "PSTN_TO_APP", "APP_TO_PSTN"],
                "appFirst": true,
                "customerMaySelectPublicRoute": false,
                "publicCallingEnabled": true,
                "callbackRelayEnabled": false,
                "requireVerifiedContacts": true,
                "destinations": { "allow": ["NG", "US"], "deny": [], "default": "DENY" },
                "maxFailoverAttempts": 0,
                "duplicateRisk": "BLOCK_UNTIL_RECONCILED",
            })
            .to_string(),
        )
        .unwrap();
        let policy = policy.display().to_string();
        let credential = c.credential_id.to_string();
        for args in [
            [&["routing-policy", "set"][..], &scope, &["--file", &policy]].concat(),
            [
                &["spend-policy", "set"][..],
                &scope,
                &[
                    "--currency",
                    "USD",
                    "--leg-allowance-minor",
                    "100",
                    "--daily-limit-minor",
                    "100000",
                    "--monthly-limit-minor",
                    "1000000",
                ],
            ]
            .concat(),
            [
                &["inbound-owner", "set"][..],
                &scope,
                &["--credential", &credential],
            ]
            .concat(),
        ] {
            run_cli(&self.bins.calling_admin, &args, &env);
        }
    }

    /// A registered application endpoint of `principal`.
    async fn endpoint(&self, token: &str, principal: Uuid, endpoint_key: &str) -> Uuid {
        let (status, r) = self
            .api(
                token,
                Method::POST,
                "/v1/calling/endpoints",
                Some(json!({
                    "principalId": principal, "endpointKey": endpoint_key, "endpointKind": "MOBILE",
                    "capabilities": { "audio": true, "video": false },
                })),
            )
            .await;
        assert!(status.is_success(), "{status}: {r}");
        r["endpointId"].as_str().unwrap().parse().unwrap()
    }

    /// An ACTIVE voice DCN: bound in Telephony by its operator, its VOICE route
    /// acknowledged by Telephony itself (only while Telephony can route it), activated.
    #[allow(clippy::too_many_arguments)]
    async fn voice_dcn(
        &self,
        s: &OwnerScope,
        external_ref: &str,
        number_type: NumberType,
        caps: &[C],
        class: O,
        routing: RoutingTargets,
    ) -> (Uuid, AssignmentRow) {
        let (id, a) = self
            .numbers
            .prepare(s, external_ref, number_type, caps, class, None, routing)
            .await;
        self.bind(id).unwrap();
        self.numbers.acknowledge(id, &["SMS"]).await;
        eventually(
            self.numbers.h.conn(),
            &format!(
                "SELECT count(*)::text FROM number_route_readiness r \
                 JOIN number_route_configurations c ON c.id = r.route_configuration_id \
                 WHERE c.number_resource_id = '{id}' AND c.family = 'VOICE' AND r.status = 'READY'"
            ),
            "1",
        )
        .await;
        (id, self.numbers.activate(s, &a).await)
    }

    /// `telephony-admin dcn bind` (the canonical number is read from Numbering).
    fn bind(&self, id: Uuid) -> Result<String, String> {
        let mut c = std::process::Command::new(&self.bins.telephony_admin);
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .args([
                "dcn",
                "bind",
                "--number-resource",
                &id.to_string(),
                "--route",
                "ng-a",
            ]);
        for (k, v) in &self.telephony_admin_env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(format!(
                "{}{}",
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            ))
        }
    }

    /// A carrier call to `called` from `caller` that the platform must refuse (403 from
    /// the edge); returns the SIP messages exchanged.
    async fn refused_call(&self, called: &str, caller: &str) -> String {
        let mut sipp = Sipp::call("carrier_uac_refused.xml", called, caller);
        let ok = sipp.finished_ok().await;
        let messages = sipp.messages();
        assert!(
            ok,
            "the call was not refused:\n{messages}\n--- edge ---\n{}",
            self.asterisk.console()
        );
        assert!(messages.contains("SIP/2.0 403"), "{messages}");
        messages
    }

    async fn metric(&self, series: &str) -> f64 {
        let text = self
            .http
            .get(&self.calling_metrics)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        metric_value(&text, series)
    }

    /// Waits until the Calling metric `series` exceeds `above`.
    async fn metric_above(&self, series: &str, above: f64) -> f64 {
        for _ in 0..200 {
            let v = self.metric(series).await;
            if v > above {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{series} never exceeded {above}");
    }

    /// Numbering's resolution of `e164` for Telephony (exactly Telephony's request).
    async fn resolve(&self, e164: &str) -> Value {
        let h = &self.numbers.h;
        h.internal_json(
            &h.peers.telephony,
            Method::POST,
            "/internal/v1/routing/inbound-resolutions",
            json!({ "family": "VOICE", "e164": e164 }),
        )
        .await
        .body
    }

    /// An inbound DCN offer to Calling, authenticated as Telephony.
    async fn offer(&self, body: &Value) -> StatusCode {
        let signed = self
            .numbers
            .h
            .peers
            .telephony
            .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
                cf_service_auth::ServiceId::calling(),
            ))
            .unwrap();
        let mut r = self
            .http
            .post(format!(
                "{}/internal/v1/telephony/dcn-inbound-calls",
                self.calling_url
            ))
            .json(body);
        for (name, value) in signed.headers().iter() {
            r = r.header(name.as_str(), value.to_str().unwrap());
        }
        StatusCode::from_u16(r.send().await.unwrap().status().as_u16()).unwrap()
    }

    async fn grants_for(&self, id: Uuid, target: Uuid) -> i64 {
        one(
            self.numbers.h.conn(),
            &format!(
                "SELECT count(*)::text FROM number_routing_grants WHERE number_resource_id = '{id}' \
                 AND grant_kind = 'INBOUND_VOICE' AND target_ref = '{target}'"
            ),
        )
        .await
        .parse()
        .unwrap()
    }

    async fn protected_callers(&self) -> i64 {
        one(
            &self.relay_db,
            "SELECT count(*)::text FROM protected_contacts",
        )
        .await
        .parse()
        .unwrap()
    }
}

impl VoiceStack {
    async fn call(&self, token: &str, id: &str) -> Value {
        let (status, view) = self
            .api(token, Method::GET, &format!("/v1/calls/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        view
    }

    /// Waits until the call satisfies `done` (polling Calling's public API).
    async fn until_call(
        &self,
        token: &str,
        id: &str,
        what: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        for _ in 0..400 {
            let view = self.call(token, id).await;
            if done(&view) {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "call {id} never reached: {what}: {}\n--- edge ---\n{}",
            self.call(token, id).await,
            self.asterisk.console()
        );
    }

    /// The newest PUBLIC_INBOUND call's id, once Calling has one more than `before`.
    async fn next_inbound_call(&self, before: i64, caller: &Sipp) -> String {
        for _ in 0..300 {
            if self.inbound_calls().await > before {
                return one(
                    &self.calling_db,
                    "SELECT id::text FROM calls WHERE origin = 'PUBLIC_INBOUND' ORDER BY created_at DESC LIMIT 1",
                )
                .await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let incidents = rows(
            &self.telephony_db,
            "SELECT category FROM telephony_incidents ORDER BY 1",
        )
        .await;
        panic!(
            "no inbound call reached Calling\n--- carrier ---\n{}\n--- incidents ---\n{incidents:?}\n--- edge ---\n{}",
            caller.messages(),
            self.asterisk.console()
        );
    }

    async fn inbound_calls(&self) -> i64 {
        one(
            &self.calling_db,
            "SELECT count(*)::text FROM calls WHERE origin = 'PUBLIC_INBOUND'",
        )
        .await
        .parse()
        .unwrap()
    }

    async fn telephony_legs(&self) -> i64 {
        one(
            &self.telephony_db,
            "SELECT count(*)::text FROM telephony_legs",
        )
        .await
        .parse()
        .unwrap()
    }

    /// `endpoint`'s RINGING invitation of the call view, if any.
    fn invitation(view: &Value, endpoint: Uuid) -> Option<Value> {
        view["invitations"]
            .as_array()?
            .iter()
            .find(|i| i["endpointId"] == endpoint.to_string() && i["status"] == "RINGING")
            .cloned()
    }

    /// The application accepts the call on `endpoint` (the existing invitation path).
    async fn accept(&self, token: &str, id: &str, endpoint: Uuid) -> (StatusCode, Value) {
        let view = self.call(token, id).await;
        let invitation =
            Self::invitation(&view, endpoint).unwrap_or_else(|| panic!("no invitation: {view}"));
        self.api(
            token,
            Method::POST,
            &format!("/v1/calls/{id}/accept"),
            Some(json!({
                "invitationId": invitation["invitationId"],
                "endpointId": endpoint,
                "expectedVersion": view["version"],
            })),
        )
        .await
    }

    /// The application endpoint joins the call's media with a Calling-issued credential
    /// (its session on `endpoint`), over a real WebRTC client.
    async fn join_media(&self, token: &str, id: &str, endpoint: Uuid) -> RtcApp {
        let view = self.call(token, id).await;
        let session = view["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["endpointId"] == endpoint.to_string())
            .unwrap_or_else(|| panic!("no session on the endpoint: {view}"))["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let (status, credential) = self
            .api(
                token,
                Method::POST,
                &format!("/v1/calls/{id}/sessions/{session}/media-credentials"),
                Some(json!({})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{credential}");
        let mut app = RtcApp::spawn(&self.bins.rtc_python);
        let joined = app
            .cmd(
                json!({ "cmd": "connect", "url": credential["url"], "token": credential["token"] }),
            )
            .await;
        if joined["ok"] != true {
            let metrics = self
                .http
                .get(&self.calling_metrics)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let admission: Vec<&str> = metrics
                .lines()
                .filter(|l| l.contains("admission") || l.contains("gateway"))
                .collect();
            panic!(
                "the application joins media: {joined}\n{}",
                admission.join("\n")
            );
        }
        app
    }

    async fn end(&self, token: &str, id: &str) -> (StatusCode, Value) {
        let view = self.call(token, id).await;
        self.api(
            token,
            Method::POST,
            &format!("/v1/calls/{id}/end"),
            Some(json!({ "expectedVersion": view["version"] })),
        )
        .await
    }

    /// A protected, VERIFIED contact for `number` (Calling's Contact Vault access grant,
    /// then Privacy Relay's vault API).
    async fn protected_contact(&self, token: &str, number: &str) -> String {
        let (status, grant) = self
            .api(
                token,
                Method::POST,
                "/v1/privacy-relay/contact-access-grants",
                Some(json!({ "scopes": ["privacy.contacts.read", "privacy.contacts.write", "privacy.contacts.verify"] })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{grant}");
        let bearer = format!("Bearer {}", grant["grant"]["accessToken"].as_str().unwrap());
        let created: Value = self
            .http
            .post(format!("{}/v1/privacy-relay/contacts", self.relay_url))
            .header("X-CF-Contact-Authorization", &bearer)
            .json(&json!({ "telephoneNumber": number }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let contact = created["contact"]["contactReference"]
            .as_str()
            .unwrap_or_else(|| panic!("{created}"))
            .to_string();
        let verified = self
            .http
            .post(format!("{}/v1/privacy-relay/contacts/{contact}/verification", self.relay_url))
            .header("X-CF-Contact-Authorization", &bearer)
            .json(&json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "OTP_PROVIDER" }))
            .send()
            .await
            .unwrap();
        assert_eq!(verified.status().as_u16(), 200);
        contact
    }

    /// A communication context of `principal` and `contact` with a PUBLIC_RELAY_ALLOWED
    /// session (Privacy Relay's authority over the public leg).
    async fn public_session(&self, token: &str, principal: Uuid, contact: &str) -> Uuid {
        let now = Utc::now();
        let (status, context) = self
            .api(
                token,
                Method::POST,
                "/v1/privacy-relay/contexts",
                Some(json!({
                    "externalContextReference": format!("e2e-{}", Uuid::new_v4().simple()),
                    "participants": [{ "principalId": principal }, { "contactReferenceId": contact }],
                    "allowedChannels": ["VOICE"],
                    "directionPolicy": "BIDIRECTIONAL",
                    "allowedRelayStrategies": ["APP_ONLY", "PUBLIC_RELAY_ALLOWED"],
                    "startsAt": (now - chrono::Duration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "expiresAt": (now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{context}");
        let id = context["context"]["contextId"].as_str().unwrap();
        let (status, session) = self
            .api(
                token,
                Method::POST,
                &format!("/v1/privacy-relay/contexts/{id}/sessions"),
                Some(json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{session}");
        session["session"]["relaySessionId"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }
}

/// Scenarios A, B, E and the voice parts of F and G — carrier calls over real SIP (SIPp ->
/// the Asterisk edge) into the real Telephony, Privacy Relay, Numbering and Calling
/// processes, Calling's media on a real LiveKit server whose SIP service bridges the
/// application into the edge.
///
/// The environment is a SANDBOX and every number is Numbering's SYNTHETIC inventory: the
/// calls run over Telephony's synthetic TEST interconnect (`ng-a`: the loopback edge, SIPp
/// as the carrier, zero cost), enabled in Telephony and Calling for this local / test
/// runtime only. Paid public routes stay refused for the sandbox. Nothing reaches a carrier.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voice_journeys_across_the_sip_edge_telephony_privacy_relay_numbering_and_calling() {
    let Some(st) = VoiceStack::start().await else {
        return;
    };
    let me = st.customer().await;
    let token = me.secret.clone();
    let s = scope_of(&me);
    let employee = st.pa.principal(me.environment_id).await;
    let replacement = st.pa.principal(me.environment_id).await;
    st.configure_calling(&me).await;
    let employee_endpoint = st.endpoint(&token, employee, "employee-desk").await;
    let replacement_endpoint = st.endpoint(&token, replacement, "replacement-desk").await;
    const LIVE_TELECOM: &str =
        "calling_public_route_denials_total{reason=\"LIVE_TELECOM_NOT_ALLOWED\"}";
    const MISMATCH: &str = "calling_dcn_refusals_total{reason=\"GRANT_MISMATCH\"}";
    const REFUSED: &str = "calling_dcn_refusals_total{reason=\"GRANT_REFUSED\"}";
    const INTERCONNECT: &str = "calling_dcn_refusals_total{reason=\"INTERCONNECT_MISMATCH\"}";

    // ---- Scenario A: an inbound call to an employee's direct DCN rings the employee's
    //      application endpoint; the application accepts; the caller hangs up.
    let (dcn, assignment) = st
        .voice_dcn(
            &s,
            "syn-voice-only",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound],
            O::EmployeeDirect,
            RoutingTargets {
                voice: (V::Principal, Some(employee)),
                sms: (Sms::None, None),
            },
        )
        .await;
    let e164 = st.numbers.e164(dcn).await;
    let before = st.inbound_calls().await;
    let mut caller = Sipp::call("carrier_uac_inbound.xml", &e164, "+2348035557001");
    let call_a = st.next_inbound_call(before, &caller).await;
    let ringing = st
        .until_call(&token, &call_a, "ringing the employee", |v| {
            VoiceStack::invitation(v, employee_endpoint).is_some()
        })
        .await;
    assert!(
        VoiceStack::invitation(&ringing, replacement_endpoint).is_none(),
        "only the current owner's endpoint rings: {ringing}"
    );
    // Numbering resolved THIS call (grant for the employee at the current revision);
    // Privacy Relay protected the caller under it; Telephony's leg carries the DCN
    // evidence and the synthetic route.
    assert_eq!(st.grants_for(dcn, employee).await, 1);
    assert_eq!(st.protected_callers().await, 1);
    let revision = one(
        st.numbers.h.conn(),
        &format!("SELECT routing_revision::text FROM number_resources WHERE id = '{dcn}'"),
    )
    .await;
    assert_eq!(
        one(
            &st.telephony_db,
            &format!(
                "SELECT l.status || '/' || l.route_class || '/' || l.dcn_number_resource_id::text || '/' || \
                 l.dcn_routing_revision || '/' || r.interconnect_class FROM telephony_legs l \
                 JOIN telecom_routes r ON r.id = l.route_id WHERE l.calling_call_id = '{call_a}'"
            )
        )
        .await,
        format!("RINGING/PSTN_TO_APP/{dcn}/{revision}/SYNTHETIC_TEST")
    );
    assert_eq!(
        one(
            &st.calling_db,
            &format!("SELECT route_plan ->> 'interconnectClass' FROM call_routes WHERE call_id = '{call_a}'")
        )
        .await,
        "SYNTHETIC_TEST"
    );
    let (status, accepted) = st.accept(&token, &call_a, employee_endpoint).await;
    assert!(status.is_success(), "{status}: {accepted}");
    let mut app_a = st.join_media(&token, &call_a, employee_endpoint).await;
    // Telephony answers the carrier, LiveKit SIP joins the edge bridge with the single-use
    // code, and Calling records the call connected only on Telephony's evidence.
    st.until_call(&token, &call_a, "connected", |v| v["state"] == "CONNECTED")
        .await;
    // The application hears the carrier: LiveKit SIP publishes the public leg's audio.
    let heard = app_a
        .cmd(json!({ "cmd": "wait_subscribed", "count": 1, "timeout": 10 }))
        .await;
    assert_eq!(
        heard["ok"], true,
        "the application subscribes the bridged audio: {heard}"
    );
    assert_eq!(
        one(
            &st.telephony_db,
            &format!("SELECT count(*)::text FROM telephony_bridges WHERE calling_call_id = '{call_a}' AND status = 'ACTIVE'")
        )
        .await,
        "1"
    );
    println!("A: ringing -> accepted -> connected (LiveKit SIP bridged into the edge)");
    // The caller hangs up after a few seconds of audio: the call completes.
    assert!(
        caller.finished_ok().await,
        "{}\n--- edge ---\n{}",
        caller.messages(),
        st.asterisk.console()
    );
    let sip = caller.messages();
    let (r180, r200) = (
        sip.find("SIP/2.0 180").unwrap(),
        sip.find("SIP/2.0 200").unwrap(),
    );
    assert!(r180 < r200, "ringing before the answer");
    let ended = st
        .until_call(&token, &call_a, "ended", |v| v["state"] == "ENDED")
        .await;
    println!("A: completed: {}", ended["terminalReason"]);
    // Records: Calling's public and application legs; Telephony's leg ended, one usage
    // record on the zero-cost synthetic route — no carrier, no charge.
    assert_eq!(
        one(
            &st.calling_db,
            &format!("SELECT string_agg(leg_kind::text, ',' ORDER BY leg_kind::text) FROM call_legs WHERE call_id = '{call_a}'")
        )
        .await,
        "APPLICATION,PUBLIC_NETWORK"
    );
    eventually(
        &st.telephony_db,
        &format!(
            "SELECT count(*)::text FROM telephony_usage_records WHERE calling_call_id = '{call_a}' \
             AND coalesce(estimated_cost_minor, 0) = 0 AND coalesce(provider_cost_minor, 0) = 0 AND route_id = 'ng-a'"
        ),
        "1",
    )
    .await;
    println!("A: inbound DCN call completed end to end over the synthetic test interconnect");

    // ---- Scenario B: the employee calls out presenting the DCN.
    let mut carrier = Sipp::serve("carrier_uas_answer_simple.xml");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let contact = st.protected_contact(&token, "+2348031234567").await;
    let session = st.public_session(&token, employee, &contact).await;
    let outbound = |presented: Option<Uuid>, extra: Option<(&'static str, Value)>| {
        let mut body = json!({
            "kind": "VOICE",
            "initiatorPrincipalId": employee,
            "initiatorEndpointId": employee_endpoint,
            "targetContactReference": contact,
            "privacyRelaySessionId": session,
        });
        if let Some(n) = presented {
            body["presentedNumberResourceId"] = json!(n);
        }
        if let Some((k, v)) = extra {
            body[k] = v;
        }
        body
    };
    // Arbitrary caller-ID substitution is refused at the API.
    let legs = st.telephony_legs().await;
    let (status, r) = st
        .api(
            &token,
            Method::POST,
            "/v1/calls",
            Some(outbound(
                Some(dcn),
                Some(("callerId", json!("+15550109999"))),
            )),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{r}");
    // Without a DCN, the call is a paid public call: a sandbox may not make it.
    let denied = st.metric(LIVE_TELECOM).await;
    let (status, r) = st
        .api(
            &token,
            Method::POST,
            "/v1/calls",
            Some(outbound(None, None)),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{r}");
    st.metric_above(LIVE_TELECOM, denied).await;
    assert_eq!(st.telephony_legs().await, legs, "nothing reached Telephony");
    // The authorized DCN: Numbering's caller-identity grant, verified and consumed by
    // Telephony, presented to the carrier on the synthetic route.
    let (status, created) = st
        .api(
            &token,
            Method::POST,
            "/v1/calls",
            Some(outbound(Some(dcn), None)),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let call_b = created["call"]["callId"].as_str().unwrap().to_string();
    let _app_b = st.join_media(&token, &call_b, employee_endpoint).await;
    st.until_call(&token, &call_b, "connected", |v| v["state"] == "CONNECTED")
        .await;
    let invite = carrier.messages();
    let from = invite
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("from:"))
        .unwrap_or_else(|| panic!("no INVITE reached the carrier: {invite}"));
    let digits = e164.trim_start_matches('+');
    assert!(
        from.contains(&format!("sip:{e164}@")) || from.contains(&format!("sip:{digits}@")),
        "the carrier sees the authorized DCN as the caller: {from}"
    );
    println!("B: carrier received {}", from.trim());
    assert_eq!(
        one(
            &st.numbers.h.conn().clone(),
            &format!(
                "SELECT count(*)::text FROM number_routing_grants WHERE number_resource_id = '{dcn}' \
                 AND grant_kind = 'CALLER_IDENTITY' AND principal_id = '{employee}' AND consumed_at IS NOT NULL"
            )
        )
        .await,
        "1",
        "one caller-identity grant, consumed by Telephony"
    );
    assert_eq!(
        one(
            &st.telephony_db,
            &format!(
                "SELECT l.direction || '/' || l.dcn_number_resource_id::text || '/' || (l.dcn_grant_id IS NOT NULL) || '/' || \
                 (l.relay_assignment_ref IS NULL) || '/' || r.interconnect_class FROM telephony_legs l \
                 JOIN telecom_routes r ON r.id = l.route_id WHERE l.calling_call_id = '{call_b}'"
            )
        )
        .await,
        format!("OUTBOUND/{dcn}/true/true/SYNTHETIC_TEST")
    );
    println!("B: application -> Calling -> Numbering caller grant -> Telephony -> SIP carrier sees the DCN");
    let (status, r) = st.end(&token, &call_b).await;
    assert!(status.is_success(), "{status}: {r}");
    assert!(
        carrier.finished_ok().await,
        "{}\n--- edge ---\n{}",
        carrier.messages(),
        st.asterisk.console()
    );
    st.until_call(&token, &call_b, "ended", |v| v["state"] == "ENDED")
        .await;
    eventually(
        &st.telephony_db,
        &format!(
            "SELECT count(*)::text FROM telephony_usage_records WHERE calling_call_id = '{call_b}' \
             AND coalesce(estimated_cost_minor, 0) = 0 AND coalesce(provider_cost_minor, 0) = 0"
        ),
        "1",
    )
    .await;
    // Someone the number does not route to may not present it (no grant, nothing sent).
    let other_session = st.public_session(&token, replacement, &contact).await;
    let legs = st.telephony_legs().await;
    let (status, r) = st
        .api(
            &token,
            Method::POST,
            "/v1/calls",
            Some(json!({
                "kind": "VOICE",
                "initiatorPrincipalId": replacement,
                "initiatorEndpointId": replacement_endpoint,
                "targetContactReference": contact,
                "privacyRelaySessionId": other_session,
                "presentedNumberResourceId": dcn,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{r}");
    assert_eq!(r["error"]["code"], "PUBLIC_ROUTE_DENIED");
    assert_eq!(st.telephony_legs().await, legs);
    println!("B: completed; caller-ID substitution, an unentitled presenter and a paid sandbox call are refused");

    // ---- Untrusted claims: the offer's scope / target must be exactly Numbering's grant,
    //      and a synthetic number never runs over a (claimed) public route.
    let live = st.resolve(&e164).await;
    assert_eq!(live["outcome"], "ROUTED", "{live}");
    assert_eq!(live["inventoryClass"], "SYNTHETIC");
    let stranger = st.customer().await;
    let mismatches = st.metric(MISMATCH).await;
    let mut forged = offer_of(&live);
    forged["scope"]["tenantId"] = json!(stranger.tenant_id);
    forged["scope"]["applicationId"] = json!(stranger.application_id);
    forged["scope"]["environmentId"] = json!(stranger.environment_id);
    assert_eq!(st.offer(&forged).await, StatusCode::FORBIDDEN);
    let mut forged = offer_of(&live);
    forged["dcnResolution"]["target"]["ref"] = json!(replacement);
    assert_eq!(st.offer(&forged).await, StatusCode::FORBIDDEN);
    assert_eq!(st.metric(MISMATCH).await, mismatches + 2.0);
    let calls = st.inbound_calls().await;
    let mismatched = st.metric(INTERCONNECT).await;
    assert_eq!(
        st.offer(&offer_of(&live)).await,
        StatusCode::FORBIDDEN,
        "PUBLIC claimed"
    );
    st.metric_above(INTERCONNECT, mismatched).await;
    assert_eq!(st.inbound_calls().await, calls);
    println!("G: a substituted scope (wrong tenant) or target, and a synthetic number on a public route, are refused");

    // ---- Scenario F (voice): an expired grant is refused by Numbering's verification.
    let refused = st.metric(REFUSED).await;
    st.numbers
        .grant_skew
        .store(400, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!st.offer(&offer_of(&live)).await.is_success());
    st.numbers
        .grant_skew
        .store(0, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(st.metric(REFUSED).await, refused + 1.0);
    println!("F: an expired routing grant is refused");

    // ---- Scenario E: the employee leaves; the number stays the organization's and now
    //      rings the replacement. Grants and presentation for the old routing stop.
    let moved = st
        .numbers
        .a
        .offboard_employee(
            &ctx(),
            &s,
            assignment.id,
            st.numbers.routing_revision(dcn).await,
            Some(RoutingTargets {
                voice: (V::Principal, Some(replacement)),
                sms: (Sms::None, None),
            }),
        )
        .await
        .unwrap();
    assert_eq!(moved.id, assignment.id);
    assert_eq!(moved.ownership_class, "EMPLOYEE_DIRECT");
    assert_eq!(moved.status, "ACTIVE");
    let refused = st.metric(REFUSED).await;
    assert!(!st.offer(&offer_of(&live)).await.is_success());
    assert_eq!(st.metric(REFUSED).await, refused + 1.0);
    let before = st.inbound_calls().await;
    let mut caller = Sipp::call("carrier_uac_cancel.xml", &e164, "+2348035557002");
    let call_e = st.next_inbound_call(before, &caller).await;
    let ringing = st
        .until_call(&token, &call_e, "ringing the replacement", |v| {
            VoiceStack::invitation(v, replacement_endpoint).is_some()
        })
        .await;
    assert!(
        VoiceStack::invitation(&ringing, employee_endpoint).is_none(),
        "never the former employee"
    );
    assert_eq!(st.grants_for(dcn, replacement).await, 1);
    assert!(caller.finished_ok().await, "{}", caller.messages());
    let cancelled = st
        .until_call(&token, &call_e, "cancelled by the caller", |v| {
            v["state"] == "NO_ANSWER"
        })
        .await;
    assert_eq!(
        cancelled["invitations"][0]["status"], "CANCELLED",
        "{cancelled}"
    );
    // The former employee can no longer present the number.
    let legs = st.telephony_legs().await;
    let (status, r) = st
        .api(
            &token,
            Method::POST,
            "/v1/calls",
            Some(outbound(Some(dcn), None)),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{r}");
    assert_eq!(st.telephony_legs().await, legs);
    println!("E: offboarded -> old grant refused, the next call rings the replacement only, the former employee cannot present the number, ownership stays organizational");

    // ---- Numbering unavailable: DCN calls fail closed at the edge (before Privacy Relay
    //      or Calling), while application calls keep working.
    let calls = st.inbound_calls().await;
    let callers = st.protected_callers().await;
    st.numbering_offline
        .store(true, std::sync::atomic::Ordering::SeqCst);
    st.refused_call(&e164, "+2348035557003").await;
    assert_eq!(st.protected_callers().await, callers);
    let (status, call) = st
        .api(
            &token,
            Method::POST,
            "/v1/calls",
            Some(json!({
                "kind": "VOICE",
                "initiatorPrincipalId": employee,
                "initiatorEndpointId": employee_endpoint,
                "targetPrincipalIds": [replacement],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{call}");
    st.numbering_offline
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        one(
            &st.telephony_db,
            "SELECT count(*)::text FROM telephony_incidents WHERE category = 'NUMBERING_UNAVAILABLE'"
        )
        .await,
        "1"
    );
    assert_eq!(st.inbound_calls().await, calls);
    println!("G: Numbering down -> DCN call refused at the edge (incident), app-to-app call still created");

    // ---- Scenario F (voice): capability downgrade — inbound voice revoked mid-life.
    st.numbers
        .h
        .numbers
        .revoke_capability(
            &ctx(),
            numbering::numbers::Actor::operator("ops.e2e"),
            dcn,
            C::VoiceInbound,
            "OPERATOR_REVOKED",
        )
        .await
        .unwrap();
    let grants = st.grants_for(dcn, replacement).await;
    st.refused_call(&e164, "+2348035557004").await;
    assert_eq!(
        st.grants_for(dcn, replacement).await,
        grants,
        "no grant after the downgrade"
    );
    assert_eq!(st.protected_callers().await, callers);
    println!("F: inbound voice capability revoked -> refused at the edge, no grant, caller never reaches Privacy Relay");

    // ---- Scenario G (voice): suspended, releasing and quarantined numbers.
    let (geo, a2) = st
        .voice_dcn(
            &s,
            "syn-voice-sms-geo",
            NumberType::Geographic,
            &[C::VoiceInbound, C::VoiceOutbound],
            O::EmployeeDirect,
            RoutingTargets {
                voice: (V::Principal, Some(replacement)),
                sms: (Sms::None, None),
            },
        )
        .await;
    let geo_e164 = st.numbers.e164(geo).await;
    let geo_live = st.resolve(&geo_e164).await;
    assert_eq!(geo_live["outcome"], "ROUTED", "{geo_live}");
    let actor = customer_actor(&s);
    let a2 = st
        .numbers
        .h
        .numbers
        .set_assignment_suspended(
            &ctx(),
            actor.clone(),
            a2.id,
            a2.revision,
            true,
            "CUSTOMER_SUSPENDED",
        )
        .await
        .unwrap();
    let refused = st.metric(REFUSED).await;
    assert!(!st.offer(&offer_of(&geo_live)).await.is_success());
    assert_eq!(st.metric(REFUSED).await, refused + 1.0);
    let grants = st.grants_for(geo, replacement).await;
    st.refused_call(&geo_e164, "+2348035557005").await;
    let a2 = st
        .numbers
        .h
        .numbers
        .set_assignment_suspended(
            &ctx(),
            actor.clone(),
            a2.id,
            a2.revision,
            false,
            "CUSTOMER_RESUMED",
        )
        .await
        .unwrap();
    let a2 = st
        .numbers
        .h
        .numbers
        .request_release(&ctx(), actor.clone(), a2.id, a2.revision)
        .await
        .unwrap();
    st.refused_call(&geo_e164, "+2348035557006").await;
    let a2 = st
        .numbers
        .h
        .numbers
        .remove_routing(&ctx(), actor.clone(), a2.id, a2.revision)
        .await
        .unwrap();
    let a2 = st
        .numbers
        .h
        .numbers
        .quarantine_assignment(
            &ctx(),
            actor.clone(),
            a2.id,
            a2.revision,
            Utc::now() + chrono::Duration::days(30),
        )
        .await
        .unwrap();
    assert_eq!(a2.status, "QUARANTINED");
    st.refused_call(&geo_e164, "+2348035557007").await;
    assert_eq!(
        st.grants_for(geo, replacement).await,
        grants,
        "no grant while suspended / releasing / quarantined"
    );
    assert_eq!(st.protected_callers().await, callers);
    assert_eq!(st.inbound_calls().await, calls);
    println!("G: suspended / releasing / quarantined DCN -> refused at the edge, no grant, nothing offered");

    // ---- Unsupported: a DCN without voice capability is never carried for voice.
    let team = Uuid::new_v4();
    let (sms_only, _) = st
        .numbers
        .active(
            &s,
            "syn-voice-sms-mobile",
            NumberType::Mobile,
            &[C::SmsInbound, C::SmsOutbound],
            O::SharedTeam,
            None,
            RoutingTargets {
                voice: (V::None, None),
                sms: (Sms::SharedTeam, Some(team)),
            },
        )
        .await;
    // An operator may list it in Telephony's allowlist; Numbering, the routing
    // authority, still refuses every voice resolution of it.
    st.bind(sms_only).unwrap();
    st.refused_call(&st.numbers.e164(sms_only).await, "+2348035557008")
        .await;
    assert_eq!(st.protected_callers().await, callers);
    assert_eq!(st.inbound_calls().await, calls);
    println!("G: an SMS-only DCN is refused for voice");

    // ---- Privacy: Calling never holds a DCN, caller or destination number; Telephony
    //      never stores a caller or destination; Privacy Relay holds them only sealed.
    let calling = dump(&st.calling_db).await;
    let telephony = dump(&st.telephony_db).await;
    let relay = dump(&st.relay_db).await;
    for number in [e164.as_str(), geo_e164.as_str()] {
        assert!(
            !calling.contains(number.trim_start_matches('+')),
            "Calling stores a DCN"
        );
    }
    let mut private: Vec<String> = (1..=8).map(|c| format!("234803555700{c}")).collect();
    private.push("2348031234567".to_string());
    for digits in &private {
        assert!(
            !calling.contains(digits.as_str()),
            "Calling stores {digits}"
        );
        assert!(
            !telephony.contains(digits.as_str()),
            "Telephony stores {digits}"
        );
        assert!(
            !relay.contains(digits.as_str()),
            "Privacy Relay stores {digits} in clear"
        );
    }
    println!("privacy: no DCN, caller or destination number outside its authority");
}
