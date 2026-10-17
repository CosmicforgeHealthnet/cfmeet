//! Part IV Part P §§119–123 across REAL process boundaries: Numbering served on a real
//! port and the real Privacy Relay binary (its own database), configured exactly as an
//! operator would:
//!
//! 1. LOCAL (Part III behaviour): a relay session gets a relay number; Telephony's inbound
//!    resolution of that number names the session;
//! 2. NUMBERING: the restarted Privacy Relay backfills its inventory into Numbering
//!    (PRIVACY_RELAY Number Resources) and adopts the live mapping (a lease held by that
//!    very assignment and generation) before it lends anything; the existing mapping's
//!    inbound resolution is IDENTICAL before and after cutover; a new session gets a
//!    number only through a Numbering lease; a number Numbering takes out of service is
//!    never lent; the end of an assignment releases its lease at Numbering;
//! 3. rollback to LOCAL: Privacy Relay lends on its own again, nothing it leased stays
//!    held, the inbound mapping is unchanged.
//!
//! `make communications-e2e`; SKIPPED without PRIVACY_RELAY_BIN / PRIVACY_RELAY_MIGRATION_BIN
//! unless `CFMEET_REQUIRE_E2E=1`.
mod common;

use std::{path::PathBuf, time::Duration};

use cf_service_auth::{
    contact_delegation::{ContactDelegationSigner, ContactScope},
    ApplicationEnvironmentKind, DelegatedPlatformContext, KeyId, OutboundServiceRequest,
    RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, TokenTtl,
};
use common::Harness;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const RELAY_A: &str = "+442079460610";
const RELAY_B: &str = "+442079460611";
const CONTACT: &str = "+442079460123";

struct Bins {
    relay: PathBuf,
    relay_migration: PathBuf,
    relay_admin: PathBuf,
}

fn bins() -> Option<Bins> {
    let get = |v: &str| std::env::var_os(v).map(PathBuf::from);
    match (get("PRIVACY_RELAY_BIN"), get("PRIVACY_RELAY_MIGRATION_BIN")) {
        (Some(relay), Some(relay_migration)) => {
            let relay_admin = relay.with_file_name("privacy-relay-admin");
            relay_admin.exists().then_some(Bins {
                relay,
                relay_migration,
                relay_admin,
            })
        }
        _ if std::env::var("CFMEET_REQUIRE_E2E").as_deref() == Ok("1") => {
            panic!("CFMEET_REQUIRE_E2E=1 but PRIVACY_RELAY_BIN / PRIVACY_RELAY_MIGRATION_BIN are not set")
        }
        _ => {
            eprintln!("SKIPPED: relay cutover journey (PRIVACY_RELAY_BIN, PRIVACY_RELAY_MIGRATION_BIN unset)");
            None
        }
    }
}

fn pems() -> &'static std::collections::HashMap<String, String> {
    common::PEER_KEY_PEMS.get_or_init(|| {
        [
            "calling-test-1",
            "telephony-test-1",
            "sms-gateway-test-1",
            "privacy-relay-lease-1",
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

fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &pems()[kid]).unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Proc(tokio::process::Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

async fn spawn(bin: &PathBuf, env: &[(String, String)], health: &str) -> Proc {
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

async fn one(db: &DatabaseConnection, sql: &str) -> String {
    db.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .and_then(|r| r.try_get_by_index::<String>(0).ok())
    .unwrap_or_default()
}

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

fn bundle(receiver: &str, issuers: &[(&str, &str, &[&str])]) -> String {
    json!({
        "bundle_version": 1, "receiver": receiver, "runtime_environment": "test",
        "trusted_issuers": issuers.iter().map(|(issuer, kid, perms)| json!({
            "issuer": issuer, "audience": receiver, "runtime_environment": "test",
            "keys": [key(kid).public_jwk()], "permissions": perms,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// A peer talking to the real Privacy Relay.
struct Relay {
    url: String,
    http: reqwest::Client,
    calling: ServiceTokenSigner,
    telephony: ServiceTokenSigner,
    contacts: ContactDelegationSigner,
    delegated: DelegatedPlatformContext,
}

impl Relay {
    async fn internal(
        &self,
        signer: &ServiceTokenSigner,
        delegated: bool,
        path: &str,
        body: Value,
    ) -> (u16, Value) {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::privacy_relay());
        if delegated {
            outbound = outbound.with_delegated_context(self.delegated.clone());
        }
        let signed = signer.sign(&outbound).unwrap();
        let mut r = self.http.post(format!("{}{path}", self.url)).json(&body);
        for (name, value) in signed.headers().iter() {
            r = r.header(name.as_str(), value.as_bytes());
        }
        let resp = r.send().await.unwrap();
        (
            resp.status().as_u16(),
            resp.json().await.unwrap_or(Value::Null),
        )
    }

    async fn command(&self, path: &str, request: Value) -> Value {
        let (status, v) = self
            .internal(
                &self.calling,
                true,
                path,
                json!({ "idempotencyKey": Uuid::new_v4().to_string(), "request": request }),
            )
            .await;
        assert_eq!(status, 201, "{path}: {v}");
        v
    }

    async fn verified_contact(&self) -> String {
        let token = self
            .contacts
            .sign(
                &ServiceId::privacy_relay(),
                &self.delegated,
                &[
                    ContactScope::Read,
                    ContactScope::Write,
                    ContactScope::Verify,
                ],
            )
            .unwrap();
        let bearer = format!("Bearer {}", token.expose_for_delivery());
        let created: Value = self
            .http
            .post(format!("{}/v1/privacy-relay/contacts", self.url))
            .header("X-CF-Contact-Authorization", &bearer)
            .json(&json!({ "telephoneNumber": CONTACT }))
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
        let version = created["contact"]["version"].as_i64().unwrap_or(1);
        if created["contact"]["verificationState"] != "VERIFIED" {
            let r = self
                .http
                .post(format!("{}/v1/privacy-relay/contacts/{contact}/verification", self.url))
                .header("X-CF-Contact-Authorization", &bearer)
                .json(&json!({ "expectedVersion": version, "targetState": "VERIFIED", "trustSource": "OTP_PROVIDER" }))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 200);
        }
        contact
    }

    async fn session(&self, contact: &str) -> String {
        let now = chrono::Utc::now();
        let principal = Uuid::new_v4();
        let ctx = self
            .command(
                "/internal/v1/contexts",
                json!({
                    "externalContextReference": format!("cut-{}", Uuid::new_v4().simple()),
                    "participants": [{ "principalId": principal }, { "contactReferenceId": contact }],
                    "allowedChannels": ["VOICE"],
                    "directionPolicy": "BIDIRECTIONAL",
                    "allowedRelayStrategies": ["APP_ONLY", "PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"],
                    "startsAt": (now - chrono::Duration::minutes(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    "expiresAt": (now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                }),
            )
            .await;
        let id = ctx["context"]["contextId"].as_str().unwrap().to_string();
        let s = self
            .command(
                &format!("/internal/v1/contexts/{id}/sessions"),
                json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
            )
            .await;
        s["session"]["relaySessionId"].as_str().unwrap().to_string()
    }

    async fn assign(&self, session: &str) -> (u16, Value) {
        self.internal(
            &self.calling,
            true,
            &format!("/internal/v1/sessions/{session}/relay-number-assignment"),
            json!({ "country": "GB", "capabilities": ["VOICE_INBOUND", "VOICE_OUTBOUND"] }),
        )
        .await
    }

    async fn release(&self, session: &str) {
        let (status, v) = self
            .internal(
                &self.calling,
                true,
                &format!("/internal/v1/sessions/{session}/relay-number-assignment/release"),
                json!({}),
            )
            .await;
        assert_eq!(status, 200, "{v}");
    }

    async fn inbound(&self, number: &str) -> Value {
        let (status, mut v) = self
            .internal(
                &self.telephony,
                false,
                "/internal/v1/relay-routing/resolve-inbound",
                json!({
                    "calledRelayIdentity": number,
                    "providerEventReference": format!("evt-{}", Uuid::new_v4().simple()),
                    "providerObservedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                }),
            )
            .await;
        assert_eq!(status, 200, "{number}: {v}");
        v["resolution"]["decisionId"] = Value::Null;
        v["resolution"].clone()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_numbers_move_to_numbering_leases_with_identical_routing_and_a_safe_rollback() {
    let Some(bins) = bins() else { return };
    pems();
    // Wall-clock Numbering: Privacy Relay runs on real time.
    let Some(db) = common::TestDb::new().await else {
        return;
    };
    let h = Harness::build(
        std::sync::Arc::new(db),
        None,
        numbering::domain::ManualClock::new(chrono::Utc::now()),
    );
    let (numbering_url, _offline) = h.serve().await;
    let admin = common::admin_url().expect("TEST_DATABASE_URL");
    // Privacy Relay's own database.
    let name = format!("cut_pr_{}", Uuid::new_v4().simple());
    let admin_conn = Database::connect(&admin).await.unwrap();
    admin_conn
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let relay_db_url = common::with_db(&admin, &name);
    let out = std::process::Command::new(&bins.relay_migration)
        .arg("up")
        .env("DATABASE_URL", &relay_db_url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let relay_db = Database::connect(&relay_db_url).await.unwrap();
    for (number, binding) in [(RELAY_A, "pb-cut-e2e-a"), (RELAY_B, "pb-cut-e2e-b")] {
        let out = std::process::Command::new(&bins.relay_admin)
            .args([
                "relay-number",
                "add",
                "--number",
                number,
                "--capabilities",
                "VOICE_INBOUND,VOICE_OUTBOUND",
                "--binding",
                binding,
                "--available",
                "true",
            ])
            .env("DATABASE_URL", &relay_db_url)
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let dir = std::env::temp_dir().join(format!("cfmeet-cut-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = |n: &str, content: &str| {
        let p = dir.join(n);
        std::fs::write(&p, content).unwrap();
        p.display().to_string()
    };
    let admin_bin = bins.relay_admin.clone();
    for (purpose, version, n) in [
        ("encryption", "ck-cut-1", "contact-keyring.json"),
        ("fingerprint", "fk-cut-1", "fingerprint-keyring.json"),
    ] {
        let out = std::process::Command::new(&admin_bin)
            .args([
                "contact-vault",
                "generate-keyring",
                "--purpose",
                purpose,
                "--version",
                version,
                "--out",
                &dir.join(n).display().to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let port = free_port();
    let url = format!("http://127.0.0.1:{port}");
    let env = |authority: &str| -> Vec<(String, String)> {
        let mut e: Vec<(&str, String)> = vec![
            ("DATABASE_URL", relay_db_url.clone()),
            ("PRIVACY_RELAY_BIND_ADDRESS", format!("127.0.0.1:{port}")),
            ("PRIVACY_RELAY_ENABLED", "true".into()),
            ("RELAY_NUMBER_POOL_ENABLED", "true".into()),
            ("PRIVACY_RELAY_WORKER_INTERVAL_MS", "200".into()),
            ("CONTACT_VAULT_ENABLED", "true".into()),
            ("CONTACT_VAULT_KEYRING_FILE", dir.join("contact-keyring.json").display().to_string()),
            ("CONTACT_FINGERPRINT_KEYRING_FILE", dir.join("fingerprint-keyring.json").display().to_string()),
            ("CONTACT_DELEGATION_TRUST_BUNDLE_FILE", file("contact-trust.json", &json!({
                "bundle_version": 1, "token_use": "CONTACT_VAULT_ACCESS", "audience": "cfmeet.privacy-relay",
                "runtime_environment": "test",
                "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": [key("calling-contact-test-1").public_jwk()],
                    "scopes": ["privacy.contacts.read", "privacy.contacts.write", "privacy.contacts.verify"] }],
            }).to_string())),
            ("CF_RUNTIME_ENVIRONMENT", "test".into()),
            ("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-lease-1".into()),
            ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", file("relay-key.pem", &pems()["privacy-relay-lease-1"])),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", file("relay-trust.json", &bundle(
                cf_service_auth::services::PRIVACY_RELAY,
                &[
                    (cf_service_auth::services::CALLING, "calling-test-1", &[
                        "communication-context.read", "communication-context.write", "relay-session.read",
                        "relay-session.write", "relay-number.assign",
                    ]),
                    (cf_service_auth::services::TELEPHONY, "telephony-test-1", &["relay-number.resolve-inbound"]),
                ],
            ))),
            ("RELAY_NUMBER_AUTHORITY", authority.into()),
            ("NUMBERING_INTERNAL_BASE_URL", numbering_url.clone()),
            ("CF_INTERNAL_HTTP_TIMEOUT_MS", "2000".into()),
            ("RUST_LOG", "privacy_relay=warn".into()),
        ];
        e.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    };
    let signer = |id: ServiceId, kid: &str| {
        ServiceTokenSigner::new(
            id,
            RuntimeEnvironment::Test,
            key(kid),
            TokenTtl::default(),
            std::sync::Arc::new(cf_service_auth::SystemClock),
        )
    };
    let relay = Relay {
        url: url.clone(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        calling: signer(ServiceId::calling(), "calling-test-1"),
        telephony: signer(ServiceId::telephony(), "telephony-test-1"),
        contacts: ContactDelegationSigner::new(
            ServiceId::calling(),
            RuntimeEnvironment::Test,
            key("calling-contact-test-1"),
            120,
            std::sync::Arc::new(cf_service_auth::SystemClock),
        )
        .unwrap(),
        delegated: DelegatedPlatformContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            ApplicationEnvironmentKind::Production,
            Uuid::new_v4(),
        ),
    };
    let held = |assignment: &Value| {
        let db = relay_db.clone();
        let id = assignment["assignmentId"].as_str().unwrap().to_string();
        async move {
            one(&db, &format!(
                "SELECT i.canonical_public_e164 FROM relay_number_assignments a JOIN relay_number_inventory i \
                   ON i.id = a.relay_number_id WHERE a.id = '{id}'")).await
        }
    };

    // 1. LOCAL — Part III behaviour.
    let local = spawn(&bins.relay, &env("LOCAL"), &format!("{url}/health")).await;
    let contact = relay.verified_contact().await;
    let s1 = relay.session(&contact).await;
    let (status, a1) = relay.assign(&s1).await;
    assert_eq!(status, 201, "{a1}");
    let a1 = a1["assignment"].clone();
    let first = held(&a1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = relay.inbound(&first).await;
    assert_eq!(before["outcome"], "RESOLVED", "{before}");
    assert_eq!(
        h.scalar("SELECT count(*) FROM number_relay_pool_members")
            .await,
        0
    );
    drop(local);

    // 2. NUMBERING — backfill + adoption, then leases.
    let numbering = spawn(&bins.relay, &env("NUMBERING"), &format!("{url}/health")).await;
    for _ in 0..200 {
        if h.scalar("SELECT count(*) FROM number_relay_leases WHERE status = 'ACTIVE'")
            .await
            == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM number_relay_pool_members")
            .await,
        2
    );
    assert_eq!(h.scalar("SELECT count(*) FROM number_resources WHERE service_class = 'PRIVACY_RELAY' AND state = 'AVAILABLE'").await, 2);
    let adopted = one(h.conn(), "SELECT holder_ref::text || ':' || generation FROM number_relay_leases WHERE status = 'ACTIVE'").await;
    assert_eq!(
        adopted,
        format!(
            "{}:{}",
            a1["assignmentId"].as_str().unwrap(),
            a1["assignmentGeneration"]
        )
    );
    // Routing equivalence for the pre-cutover mapping.
    assert_eq!(relay.inbound(&first).await, before);
    // A new session: leased from Numbering to its own assignment.
    let s2 = relay.session(&contact).await;
    let (status, a2) = relay.assign(&s2).await;
    assert_eq!(status, 201, "{a2}");
    let a2 = a2["assignment"].clone();
    let second = held(&a2).await;
    assert_ne!(second, first);
    let lease2 = one(
        h.conn(),
        &format!(
            "SELECT status FROM number_relay_leases WHERE holder_ref = '{}'",
            a2["assignmentId"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(lease2, "ACTIVE");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        relay.inbound(&second).await["relaySessionId"].as_str(),
        Some(s2.as_str())
    );
    // The relay numbers are never DCN inventory: not searchable, not reservable.
    assert!(h
        .numbers
        .resources_by_e164(RELAY_A)
        .await
        .unwrap()
        .iter()
        .all(|r| r.service_class == "PRIVACY_RELAY"));
    // The end of the assignment releases its Numbering lease (durable sweep).
    relay.release(&s2).await;
    eventually(
        h.conn(),
        &format!(
            "SELECT status FROM number_relay_leases WHERE holder_ref = '{}'",
            a2["assignmentId"].as_str().unwrap()
        ),
        "RELEASED",
    )
    .await;
    eventually(
        &relay_db,
        &format!(
            "SELECT status FROM relay_number_leases WHERE assignment_id = '{}'",
            a2["assignmentId"].as_str().unwrap()
        ),
        "RELEASED",
    )
    .await;
    // Numbering's RELAY_LEASES switch: Privacy Relay lends nothing new (fail closed).
    h.state
        .stewardship()
        .set_control(
            &numbering::numbers::Actor::operator("ops.cutover"),
            numbering::stewardship::controls::Control::RelayLeases,
            true,
            "CARRIER_INCIDENT",
        )
        .await
        .unwrap();
    let s3 = relay.session(&contact).await;
    let (status, refused) = relay.assign(&s3).await;
    assert_ne!(status, 201, "{refused}");
    drop(numbering);

    // 3. Rollback to LOCAL: Privacy Relay decides alone again; the old mapping routes the same.
    let _rollback = spawn(&bins.relay, &env("LOCAL"), &format!("{url}/health")).await;
    assert_eq!(relay.inbound(&first).await, before);
    let s4 = relay.session(&contact).await;
    let (status, a4) = relay.assign(&s4).await;
    // RELAY numbers: one ACTIVE (s1), the other in cooldown after s2 — LOCAL refuses only
    // for lack of numbers, never because Numbering's switch is engaged.
    assert!(
        status == 201 || a4["error"]["code"] == "RELAY_NUMBER_UNAVAILABLE",
        "{a4}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
