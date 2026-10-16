//! Part IV Step 20 — synthetic communications journeys across the ACTUAL service
//! boundaries: Numbering (this crate's real router and domain, served on a real port with
//! its own database), and REAL processes of Platform Access, the SMS Gateway, Calling,
//! Telephony and Privacy Relay — each with its own database, signed internal HTTP between
//! them, no shared database and no source imports. Customers authenticate with real
//! Platform Access credentials. Providers are the deterministic adapters (SMS provider,
//! interconnect); nothing reaches a carrier.
//!
//! Requires `TEST_DATABASE_URL`, `PLATFORM_ACCESS_BIN` / `PLATFORM_ACCESS_MIGRATION_BIN`
//! and the binaries named by [`Bins`]; without them the journeys are SKIPPED (explicit
//! line) unless `CFMEET_REQUIRE_E2E=1`.
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
    let mut c = tokio::process::Command::new(bin);
    c.env_clear()
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
        ["calling-test-1", "telephony-test-1", "sms-gateway-test-1"]
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
}

impl Numbers {
    async fn new(pa: Arc<PlatformAccess>, db: Arc<TestDb>) -> Self {
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
        let (h, grant_clock, _jwk) = h.with_grants_ttl(300);
        // The peer processes use the system clock: Numbering's clocks follow it.
        let domain_clock = h.clock.clone();
        tokio::spawn(async move {
            loop {
                let now = Utc::now();
                domain_clock.set(now);
                grant_clock.set(now.timestamp());
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        catalog::synthetic_us(&h).await;
        fx::synthetic_us(&h).await;
        let dir = Arc::new(ax::Principals::default());
        let a = ax::of(&h, &p, AcquisitionSettings::default()).with_principals(dir.clone());
        let runner = ax::runner(&h, &p, &a);
        Self { h, a, dir, runner }
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
        for r in self.h.numbers.current_routes(id).await.unwrap() {
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
        let a = self
            .a
            .activate_assignment(&ctx(), s, standing(), a.id)
            .await
            .unwrap()
            .assignment;
        assert_eq!(a.status, "ACTIVE");
        (id, a)
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
        let numbers = Numbers::new(pa.clone(), db).await;
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
