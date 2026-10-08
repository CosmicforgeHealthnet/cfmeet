//! Part E end to end across the three services: Calling (this crate, served on a real
//! port), the real `privacy-relay` binary (Contact Vault + relay-number pool) and the
//! real `telephony` binary — each with its own database, signed internal HTTP between
//! them, no shared database, no source imports. The interconnect inside Telephony is the
//! deterministic test adapter (allowed only in `local` / `test`), driven by signed
//! provider callbacks; there is no SIP edge (`TELEPHONY_SIP_EDGE` unset), so a media
//! bridge fails SAFELY — exactly the "SIP edge down" case (§205).
//!
//! Requires `PRIVACY_RELAY_BIN`, `PRIVACY_RELAY_MIGRATION_BIN`, `TELEPHONY_BIN`,
//! `TELEPHONY_MIGRATION_BIN` (with `privacy-relay-admin` / `telephony-admin` next to them)
//! and `TEST_DATABASE_URL`; skipped otherwise (`make telephony-integration`, CI job
//! `calling-e2e` of `.github/workflows/telephony.yml`).

mod common;

use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    internal_auth::ServiceAuthRuntime,
    platform::scopes::names,
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::spend::{SpendPolicy, SpendScope},
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{SecondsFormat, Utc};
use common::{app::AppWorld, TestDb, TestResponse};
use hmac::{Hmac, Mac};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::{process::Child, task::JoinHandle};
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
/// The protected destination (a test number; must never appear outside the vault).
const DESTINATION: &str = "+234 803 123 4567";
const DESTINATION_DIGITS: &str = "8031234567";
/// CF Meet's own temporary relay number (the caller identity the callee sees).
const RELAY_NUMBER: &str = "+2342010000777";
const PROVIDER_SECRET: &str = "e2e-deterministic-callback-secret-0123456789abcdef";

struct Binaries {
    relay: PathBuf,
    relay_migration: PathBuf,
    telephony: PathBuf,
    telephony_migration: PathBuf,
}

fn binaries() -> Option<Binaries> {
    Some(Binaries {
        relay: std::env::var_os("PRIVACY_RELAY_BIN")?.into(),
        relay_migration: std::env::var_os("PRIVACY_RELAY_MIGRATION_BIN")?.into(),
        telephony: std::env::var_os("TELEPHONY_BIN")?.into(),
        telephony_migration: std::env::var_os("TELEPHONY_MIGRATION_BIN")?.into(),
    })
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn run(bin: &PathBuf, args: &[&str], env: &[(&str, String)]) -> String {
    let mut c = Command::new(bin);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c
        .output()
        .unwrap_or_else(|e| panic!("{} runs: {e}", bin.display()));
    assert!(
        out.status.success(),
        "{} {args:?}: {}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

async fn wait_healthy(url: &str, what: &str) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for _ in 0..150 {
        if let Ok(r) = http.get(format!("{url}/health")).send().await {
            if r.status().is_success() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what} did not become healthy");
}

struct Proc {
    child: Child,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// The three services.
struct Stack {
    w: AppWorld,
    _relay: Proc,
    _telephony: Proc,
    relay_db: TestDb,
    telephony_db: TestDb,
    dir: PathBuf,
    calling_port: u16,
    server: Option<JoinHandle<()>>,
    /// Calling "down": every request is answered 503 (an outage as its peers see it;
    /// stopping the listener alone would leave established keep-alive connections up).
    down: Arc<std::sync::atomic::AtomicBool>,
    privacy: String,
    relay_url: String,
    telephony_url: String,
    caller: common::app::Device,
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Stack {
    async fn start() -> Option<Self> {
        let bins = binaries()?;
        let relay_db = TestDb::empty().await?;
        let telephony_db = TestDb::empty().await?;
        let dir = std::env::temp_dir().join(format!("cf-tel-e2e-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("provider-secrets")).unwrap();
        let calling_key = ServiceSigningKey::generate(KeyId::parse("calling-e2e-1").unwrap());
        let relay_key = ServiceSigningKey::generate(KeyId::parse("privacy-relay-e2e-1").unwrap());
        let telephony_key = ServiceSigningKey::generate(KeyId::parse("telephony-e2e-1").unwrap());

        // ---- Privacy Relay: Calling and Telephony with exactly their grants.
        std::fs::write(
            dir.join("relay-trust.json"),
            common::trust_bundle_json(
                cf_service_auth::services::PRIVACY_RELAY,
                ENV,
                &[
                    (
                        cf_service_auth::services::CALLING,
                        vec![calling_key.public_jwk()],
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
                        vec![telephony_key.public_jwk()],
                        &[
                            "protected-contact.resolve-for-execution",
                            "relay-number.resolve-inbound",
                            "protected-contact.protect-inbound",
                        ],
                    ),
                ],
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("relay-key.pem"),
            relay_key.to_pkcs8_pem().as_bytes(),
        )
        .unwrap();
        let admin = bins.relay.with_file_name("privacy-relay-admin");
        for (purpose, version, file) in [
            ("encryption", "ck-e2e-1", "contact-keyring.json"),
            ("fingerprint", "fk-e2e-1", "fingerprint-keyring.json"),
        ] {
            let path = dir.join(file).to_string_lossy().to_string();
            run(
                &admin,
                &[
                    "contact-vault",
                    "generate-keyring",
                    "--purpose",
                    purpose,
                    "--version",
                    version,
                    "--out",
                    &path,
                ],
                &[],
            );
        }
        let contact_signer = Arc::new(
            cf_service_auth::ContactDelegationSigner::new(
                ServiceId::calling(),
                ENV,
                ServiceSigningKey::generate(KeyId::parse("calling-contact-e2e-1").unwrap()),
                120,
                Arc::new(SystemClock),
            )
            .unwrap(),
        );
        std::fs::write(
            dir.join("contact-trust.json"),
            json!({
                "bundle_version": 1, "token_use": "CONTACT_VAULT_ACCESS", "audience": "cfmeet.privacy-relay",
                "runtime_environment": "test",
                "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": [contact_signer.public_jwk()],
                    "scopes": ["privacy.contacts.read", "privacy.contacts.write", "privacy.contacts.verify"] }],
            })
            .to_string(),
        )
        .unwrap();
        run(
            &bins.relay_migration,
            &["up"],
            &[("DATABASE_URL", relay_db.url.clone())],
        );
        // CF Meet's own relay number, bound to the interconnect route that serves it.
        run(
            &admin,
            &[
                "relay-number",
                "add",
                "--number",
                RELAY_NUMBER,
                "--capabilities",
                "VOICE_INBOUND,VOICE_OUTBOUND",
                "--binding",
                "ng-a",
                "--available",
                "true",
            ],
            &[
                ("DATABASE_URL", relay_db.url.clone()),
                ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
            ],
        );

        // ---- Telephony: its own database, the deterministic interconnect, Calling only.
        run(
            &bins.telephony_migration,
            &["up"],
            &[("DATABASE_URL", telephony_db.url.clone())],
        );
        let tadmin = bins.telephony.with_file_name("telephony-admin");
        let tenv = [
            ("DATABASE_URL", telephony_db.url.clone()),
            ("CF_RUNTIME_ENVIRONMENT", "test".to_string()),
        ];
        std::fs::write(
            dir.join("provider-secrets").join("dp-a.secret"),
            PROVIDER_SECRET,
        )
        .unwrap();
        for args in [
            vec![
                "provider",
                "add",
                "--id",
                "dp-a",
                "--adapter",
                "DETERMINISTIC_TEST",
                "--capabilities",
                "OUTBOUND_VOICE,INBOUND_VOICE,STATUS_QUERY,EVENTS,HEALTH,CAPACITY",
                "--credential-ref",
                "dp-a.secret",
            ],
            vec!["provider", "enable", "dp-a"],
            vec![
                "sip-peer",
                "add",
                "--id",
                "peer-ng-a",
                "--provider",
                "dp-a",
                "--secret-ref",
                "trunk/ng-a",
                "--allowed-cidrs",
                "192.0.2.0/24",
            ],
            vec![
                "route",
                "add",
                "--id",
                "ng-a",
                "--provider",
                "dp-a",
                "--countries",
                "NG",
                "--capabilities",
                "VOICE_OUTBOUND,VOICE_INBOUND",
                "--priority",
                "10",
                "--sip-peer",
                "peer-ng-a",
                "--estimated-leg-cost-minor",
                "100",
                "--cost-currency",
                "NGN",
            ],
            vec!["route", "enable", "ng-a"],
            vec![
                "number",
                "bind",
                "--number",
                RELAY_NUMBER,
                "--route",
                "ng-a",
            ],
        ] {
            run(&tadmin, &args, &tenv);
        }
        std::fs::write(
            dir.join("telephony-trust.json"),
            common::trust_bundle_json(
                cf_service_auth::services::TELEPHONY,
                ENV,
                &[(
                    cf_service_auth::services::CALLING,
                    vec![calling_key.public_jwk()],
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
        )
        .unwrap();
        std::fs::write(
            dir.join("telephony-key.pem"),
            telephony_key.to_pkcs8_pem().as_bytes(),
        )
        .unwrap();

        let (relay_port, telephony_port, calling_port) = (free_port(), free_port(), free_port());
        let relay_url = format!("http://127.0.0.1:{relay_port}");
        let telephony_url = format!("http://127.0.0.1:{telephony_port}");
        let calling_url = format!("http://127.0.0.1:{calling_port}");
        let relay = tokio::process::Command::new(&bins.relay)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("DATABASE_URL", &relay_db.url)
            .env(
                "PRIVACY_RELAY_BIND_ADDRESS",
                format!("127.0.0.1:{relay_port}"),
            )
            .env("PRIVACY_RELAY_ENABLED", "true")
            .env("RELAY_NUMBER_POOL_ENABLED", "true")
            .env("PRIVACY_RELAY_WORKER_INTERVAL_MS", "200")
            .env("CONTACT_VAULT_ENABLED", "true")
            .env(
                "CONTACT_VAULT_KEYRING_FILE",
                dir.join("contact-keyring.json"),
            )
            .env(
                "CONTACT_FINGERPRINT_KEYRING_FILE",
                dir.join("fingerprint-keyring.json"),
            )
            .env(
                "CONTACT_DELEGATION_TRUST_BUNDLE_FILE",
                dir.join("contact-trust.json"),
            )
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-e2e-1")
            .env(
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                dir.join("relay-key.pem"),
            )
            .env("CF_SERVICE_TRUST_BUNDLE_FILE", dir.join("relay-trust.json"))
            .env("CALLING_INTERNAL_BASE_URL", &calling_url)
            .env("CF_INTERNAL_HTTP_TIMEOUT_MS", "1000")
            .env("RUST_LOG", "privacy_relay=warn")
            .kill_on_drop(true)
            .spawn()
            .expect("privacy-relay starts");
        let relay = Proc { child: relay };
        let telephony = tokio::process::Command::new(&bins.telephony)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("DATABASE_URL", &telephony_db.url)
            .env(
                "TELEPHONY_BIND_ADDRESS",
                format!("127.0.0.1:{telephony_port}"),
            )
            .env("TELEPHONY_ENABLED", "true")
            .env("PUBLIC_OUTBOUND_CALLING_ENABLED", "true")
            .env("PUBLIC_INBOUND_CALLING_ENABLED", "true")
            .env("CALLBACK_RELAY_ENABLED", "true")
            .env("TELEPHONY_WORKER_INTERVAL_MS", "100")
            .env("TELECOM_PROVIDER_SECRETS_DIR", dir.join("provider-secrets"))
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CF_SERVICE_AUTH_KEY_ID", "telephony-e2e-1")
            .env(
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                dir.join("telephony-key.pem"),
            )
            .env(
                "CF_SERVICE_TRUST_BUNDLE_FILE",
                dir.join("telephony-trust.json"),
            )
            .env("CALLING_INTERNAL_BASE_URL", &calling_url)
            .env("PRIVACY_RELAY_INTERNAL_BASE_URL", &relay_url)
            .env("CF_INTERNAL_HTTP_TIMEOUT_MS", "1000")
            .env("RUST_LOG", "telephony=warn")
            .kill_on_drop(true)
            .spawn()
            .expect("telephony starts");
        let telephony = Proc { child: telephony };
        wait_healthy(&relay_url, "privacy-relay").await;
        wait_healthy(&telephony_url, "telephony").await;

        // ---- Calling: trusts Privacy Relay (enforcement) and Telephony (events).
        let clock: SharedClock = Arc::new(SystemClock);
        let policy = TrustPolicy::from_json(
            &common::trust_bundle_json(
                cf_service_auth::services::CALLING,
                ENV,
                &[
                    (
                        cf_service_auth::services::PRIVACY_RELAY,
                        vec![relay_key.public_jwk()],
                        &["communication-authority.enforce"],
                    ),
                    (
                        cf_service_auth::services::TELEPHONY,
                        vec![telephony_key.public_jwk()],
                        &[
                            "telephony-event.apply",
                            "telephony-reconciliation.apply",
                            "telephony-inbound.offer",
                        ],
                    ),
                ],
            ),
            &ServiceId::calling(),
            ENV,
        )
        .unwrap();
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::calling(),
            ENV,
            calling_key,
            TokenTtl::default(),
            clock.clone(),
        ));
        let mut targets = InternalTargets::new(ENV);
        targets
            .insert(ServiceId::privacy_relay(), &relay_url)
            .unwrap();
        targets
            .insert(ServiceId::telephony(), &telephony_url)
            .unwrap();
        let client = Arc::new(
            InternalServiceClient::new(signer.clone(), targets, Duration::from_secs(5)).unwrap(),
        );
        let runtime = ServiceAuthRuntime::new(
            signer,
            Arc::new(ServiceTokenVerifier::new(policy, clock)),
            client,
        )
        .unwrap();
        let (_, peers) = common::service_auth_fixture();
        let w = AppWorld::with_service_auth_public_network(
            runtime,
            peers,
            Utc::now(),
            Some(contact_signer),
        )
        .await?;
        let privacy = w
            .h()
            .issue(
                w.w.consumer.sa(w.w.kind),
                &[
                    names::PRINCIPALS_READ,
                    names::PRIVACY_CONTEXTS_READ,
                    names::PRIVACY_CONTEXTS_WRITE,
                    names::PRIVACY_SESSIONS_READ,
                    names::PRIVACY_SESSIONS_WRITE,
                    names::PRIVACY_CONTACTS_READ,
                    names::PRIVACY_CONTACTS_WRITE,
                    names::PRIVACY_CONTACTS_VERIFY,
                ],
            )
            .await
            .secret
            .expose_once()
            .to_string();
        // The environment's routing and spend policy (operator configuration).
        let scope = w.w.scope();
        let (tenant, app, env) = (
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
        );
        calling::routing::store::store_policy(
            w.h().conn(),
            tenant,
            app,
            env,
            &RoutingPolicy {
                allowed_route_classes: vec![
                    RoutingClass::AppRtc,
                    RoutingClass::AppToPstn,
                    RoutingClass::CallbackRelay,
                ],
                public_calling_enabled: true,
                callback_relay_enabled: true,
                destinations: DestinationPolicy {
                    allow: vec!["NG".into()],
                    deny: vec![],
                    default: CountryDecision::Deny,
                },
                ..RoutingPolicy::default()
            },
            Utc::now(),
        )
        .await
        .unwrap();
        calling::telephony::spend::store_policy(
            w.h().conn(),
            SpendScope {
                tenant_id: tenant,
                application_id: app,
                environment_id: env,
            },
            &SpendPolicy {
                currency: "NGN".into(),
                leg_allowance_minor: 1_000,
                max_leg_seconds: 600,
                daily_limit_minor: 100_000,
                monthly_limit_minor: 1_000_000,
                max_concurrent_legs: 10,
                max_originations_per_minute: 30,
                max_per_party_per_hour: 30,
                max_per_principal_per_hour: None,
                max_per_service_account_per_hour: None,
                max_per_context_per_hour: None,
                enabled: true,
            },
            Utc::now(),
        )
        .await
        .unwrap();
        let caller = w.device(&w.w.initiator, "caller-main").await;
        let mut stack = Self {
            w,
            _relay: relay,
            _telephony: telephony,
            relay_db,
            telephony_db,
            dir,
            calling_port,
            server: None,
            down: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            privacy,
            relay_url,
            telephony_url,
            caller,
        };
        stack.serve_calling().await;
        Some(stack)
    }

    async fn serve_calling(&mut self) {
        let mut listener = None;
        for _ in 0..50 {
            match tokio::net::TcpListener::bind(("127.0.0.1", self.calling_port)).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        let listener = listener.expect("calling port");
        let down = self.down.clone();
        let app = self.w.h().router().layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let down = down.load(std::sync::atomic::Ordering::SeqCst);
                async move {
                    if down {
                        axum::response::IntoResponse::into_response(StatusCode::SERVICE_UNAVAILABLE)
                    } else {
                        next.run(req).await
                    }
                }
            },
        ));
        self.server = Some(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
    }

    fn calling_down(&self, down: bool) {
        self.down.store(down, std::sync::atomic::Ordering::SeqCst);
    }

    async fn privacy(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        let key = Uuid::new_v4().to_string();
        let headers: Vec<(&str, &str)> = if method == Method::POST {
            vec![("idempotency-key", key.as_str())]
        } else {
            vec![]
        };
        self.w
            .h()
            .request_with(method, path, Some(&self.privacy), body, &headers)
            .await
    }

    /// A protected, VERIFIED contact for `number` (through Calling's Contact Vault
    /// access token, straight to Privacy Relay).
    async fn protected_contact(&self, number: &str) -> String {
        let grant = self
            .privacy(
                Method::POST,
                "/v1/privacy-relay/contact-access-grants",
                Some(json!({ "scopes": ["privacy.contacts.read", "privacy.contacts.write", "privacy.contacts.verify"] })),
            )
            .await;
        assert_eq!(grant.status, StatusCode::CREATED, "{}", grant.text);
        let token = format!(
            "Bearer {}",
            grant.json["grant"]["accessToken"].as_str().unwrap()
        );
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let r = http
            .post(format!("{}/v1/privacy-relay/contacts", self.relay_url))
            .header("X-CF-Contact-Authorization", &token)
            .json(&json!({ "telephoneNumber": number }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 201);
        let body: Value = r.json().await.unwrap();
        let id = body["contact"]["contactReference"]
            .as_str()
            .unwrap()
            .to_string();
        let r = http
            .post(format!("{}/v1/privacy-relay/contacts/{id}/verification", self.relay_url))
            .header("X-CF-Contact-Authorization", &token)
            .json(&json!({ "expectedVersion": 1, "targetState": "VERIFIED", "trustSource": "OTP_PROVIDER" }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 200);
        id
    }

    /// A context with the initiator and `contacts`, and a PUBLIC_RELAY_ALLOWED session.
    async fn public_session(&self, contacts: &[&str]) -> (String, Uuid) {
        let now = Utc::now();
        let mut participants = vec![json!({ "principalId": self.w.w.initiator.id() })];
        participants.extend(contacts.iter().map(|c| json!({ "contactReferenceId": c })));
        let r = self
            .privacy(
                Method::POST,
                "/v1/privacy-relay/contexts",
                Some(json!({
                    "externalContextReference": format!("delivery-{}", Uuid::new_v4().simple()),
                    "participants": participants,
                    "allowedChannels": ["VOICE"],
                    "directionPolicy": "BIDIRECTIONAL",
                    "allowedRelayStrategies": ["APP_ONLY", "PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"],
                    "startsAt": (now - chrono::Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
                    "expiresAt": (now + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
                })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let context = r.json["context"]["contextId"].as_str().unwrap().to_string();
        let r = self
            .privacy(
                Method::POST,
                &format!("/v1/privacy-relay/contexts/{context}/sessions"),
                Some(json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let session = r.json["session"]["relaySessionId"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        (context, session)
    }

    async fn call_contact(&self, contact: &str, session: Uuid) -> TestResponse {
        self.w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": contact,
                    "privacyRelaySessionId": session,
                }),
                &[("idempotency-key", &Uuid::new_v4().to_string())],
            )
            .await
    }

    async fn telephony_db(&self) -> DatabaseConnection {
        Database::connect(&self.telephony_db.url).await.unwrap()
    }

    async fn text(conn: &DatabaseConnection, sql: &str) -> Option<String> {
        conn.query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap()
        .and_then(|r| r.try_get_by_index::<Option<String>>(0).ok().flatten())
    }

    /// Waits for `sql` (a text scalar) to equal `expected` in `conn`.
    async fn until(conn: &DatabaseConnection, sql: &str, expected: &str, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let now = Self::text(conn, sql).await;
            if now.as_deref() == Some(expected) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{sql}: {now:?}, expected {expected}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_call(&self, call: Uuid, state: CallState, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let now = self.w.state(call).await.0;
            if now == state {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "call {call} is {now:?}, expected {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A provider callback, signed like the deterministic interconnect signs them.
    async fn provider_callback(&self, call_ref: &str, event: &str, sequence: i64) -> u16 {
        let body = json!({
            "eventId": format!("e2e-{}", Uuid::new_v4().simple()),
            "callRef": call_ref, "event": event, "occurredAt": Utc::now(), "sequence": sequence,
        })
        .to_string();
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(PROVIDER_SECRET.as_bytes()).unwrap();
        mac.update(body.as_bytes());
        let signature = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{}/provider-callbacks/dp-a", self.telephony_url))
            .header("content-type", "application/json")
            .header("x-cfmeet-provider-signature", signature)
            .body(body)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    /// The Telephony leg executing Calling's public leg of `call` (and its provider ref).
    async fn execution(&self, call: Uuid) -> (String, String) {
        let t = self.telephony_db().await;
        Self::until(
            &t,
            &format!("SELECT count(*)::text FROM provider_call_bindings b JOIN telephony_legs l ON l.id = b.telephony_leg_id WHERE l.calling_call_id = '{call}' AND b.status = 'BOUND'"),
            "1",
            Duration::from_secs(15),
        )
        .await;
        let leg = Self::text(
            &t,
            &format!("SELECT id::text FROM telephony_legs WHERE calling_call_id = '{call}'"),
        )
        .await
        .unwrap();
        let call_ref = Self::text(
            &t,
            &format!("SELECT provider_call_ref FROM provider_call_bindings WHERE telephony_leg_id = '{leg}'"),
        )
        .await
        .unwrap();
        (leg, call_ref)
    }

    /// Everything stored by `conn`'s database as text (tables of the public schema).
    async fn dump(conn: &DatabaseConnection, skip: &[&str]) -> String {
        let tables: Vec<String> = conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT tablename FROM pg_tables WHERE schemaname = 'public'".to_string(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.try_get_by_index::<String>(0).unwrap())
            .filter(|t| !skip.contains(&t.as_str()))
            .collect();
        let mut out = String::new();
        for t in tables {
            for r in conn
                .query_all(Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!("SELECT row_to_json(x)::text FROM \"{t}\" x"),
                ))
                .await
                .unwrap()
            {
                out.push_str(&r.try_get_by_index::<String>(0).unwrap());
                out.push('\n');
            }
        }
        out
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn app_to_phone_across_calling_privacy_relay_and_telephony() {
    let Some(s) = Stack::start().await else {
        return;
    };
    let contact = s.protected_contact(DESTINATION).await;
    let (_, session) = s.public_session(&[&contact]).await;

    // Calling: Privacy Relay authority (PUBLIC_RELAY), Telephony availability, the route
    // plan, a spend reservation, the session's relay identity, the durable ORIGINATE.
    let r = s.call_contact(&contact, session).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // Telephony resolved the destination with the single-use grant at Privacy Relay
    // (real Contact Vault decryption) and executed it on the interconnect.
    let (leg, call_ref) = s.execution(call).await;
    let t = s.telephony_db().await;
    assert_eq!(
        Stack::text(
            &t,
            &format!("SELECT destination_country FROM telephony_legs WHERE id = '{leg}'")
        )
        .await
        .as_deref(),
        Some("NG")
    );
    // Provider progress arrives at Telephony (signed callbacks) and reaches Calling as
    // normalized, authenticated events.
    assert_eq!(s.provider_callback(&call_ref, "ringing", 1).await, 200);
    s.wait_call(call, CallState::Ringing, Duration::from_secs(10))
        .await;
    // A forged callback changes nothing.
    let forged = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{}/provider-callbacks/dp-a", s.telephony_url))
        .header("x-cfmeet-provider-signature", "v1=00")
        .body(json!({ "eventId": "forged-1", "callRef": call_ref, "event": "answered", "occurredAt": Utc::now() }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status().as_u16(), 401);
    assert_eq!(s.w.state(call).await.0, CallState::Ringing);

    // Calling is down while the phone answers: Telephony keeps the event durably…
    s.calling_down(true);
    assert_eq!(s.provider_callback(&call_ref, "answered", 2).await, 200);
    Stack::until(
        &t,
        &format!("SELECT status FROM telephony_legs WHERE id = '{leg}'"),
        "ANSWERED",
        Duration::from_secs(10),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(s.w.state(call).await.0, CallState::Ringing);
    // …and Calling catches up when it is back.
    s.calling_down(false);
    // ANSWERED is transient in this stack (the bridge below fails at once), so wait for
    // Calling's durable record of the answer, not for a state a poll can miss.
    Stack::until(
        s.w.h().conn(),
        &format!(
            "SELECT CASE WHEN answered_at IS NOT NULL THEN 'answered' ELSE \
             current_state::text || '/' || coalesce(terminal_reason::text, '-') || '/' || \
             coalesce(failure_category::text, '-') END FROM calls WHERE id = '{call}'"
        ),
        "answered",
        Duration::from_secs(20),
    )
    .await;

    // Calling then asks for the application-media bridge; there is no SIP edge in this
    // stack, so the bridge fails at Telephony — safely: the Call fails, the paid provider
    // call is terminated, nothing is marked connected.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let state = s.w.state(call).await.0;
        if state.is_terminal() {
            assert_ne!(
                state,
                CallState::Ended,
                "never reported as a completed conversation"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "call stuck in {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // …as a media failure after a real answer, not a routing or provider failure.
    assert_eq!(
        Stack::text(
            s.w.h().conn(),
            &format!("SELECT failure_category::text FROM calls WHERE id = '{call}'")
        )
        .await
        .as_deref(),
        Some("MEDIA")
    );
    Stack::until(
        &t,
        &format!("SELECT CASE WHEN status IN ('ENDED', 'FAILED') THEN 'terminal' ELSE status END FROM telephony_legs WHERE id = '{leg}'"),
        "terminal",
        Duration::from_secs(20),
    )
    .await;
    // Exactly one usage fact, settled into Calling's reservation exactly once.
    Stack::until(
        &t,
        &format!(
            "SELECT count(*)::text FROM telephony_usage_records WHERE telephony_leg_id = '{leg}'"
        ),
        "1",
        Duration::from_secs(10),
    )
    .await;
    Stack::until(
        s.w.h().conn(),
        &format!("SELECT status FROM call_spend_reservations WHERE call_id = '{call}'"),
        "SETTLED",
        Duration::from_secs(10),
    )
    .await;

    // Privacy: the destination number exists nowhere but encrypted in the vault — not in
    // Calling, not in Privacy Relay's non-vault tables, not in Telephony, not in any
    // event Calling received.
    let calling_dump = Stack::dump(s.w.h().conn(), &[]).await;
    let relay_conn = Database::connect(&s.relay_db.url).await.unwrap();
    let relay_dump =
        Stack::dump(&relay_conn, &["protected_contacts", "contact_fingerprints"]).await;
    let telephony_dump = Stack::dump(&t, &[]).await;
    for (name, dump) in [
        ("calling", &calling_dump),
        ("privacy-relay", &relay_dump),
        ("telephony", &telephony_dump),
    ] {
        assert!(
            !dump.contains(DESTINATION_DIGITS),
            "{name} stores the private destination"
        );
    }
    // Calling never stores the relay number or a provider reference either.
    assert!(!calling_dump.contains("2010000777") && !calling_dump.contains(&call_ref));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_caller_cancels_and_a_revoked_context_never_reaches_a_destination() {
    let Some(s) = Stack::start().await else {
        return;
    };
    let contact = s.protected_contact(DESTINATION).await;
    let (context, session) = s.public_session(&[&contact]).await;

    // Cancel while ringing: TERMINATE reaches Telephony; the provider call ends.
    let r = s.call_contact(&contact, session).await;
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let (leg, call_ref) = s.execution(call).await;
    assert_eq!(s.provider_callback(&call_ref, "ringing", 1).await, 200);
    s.wait_call(call, CallState::Ringing, Duration::from_secs(10))
        .await;
    let (_, version) = s.w.state(call).await;
    let cancel =
        s.w.post_with(
            &format!("/v1/calls/{call}/cancel"),
            json!({ "expectedVersion": version }),
            &[("idempotency-key", &Uuid::new_v4().to_string())],
        )
        .await;
    assert!(cancel.status.is_success(), "{}", cancel.text);
    let t = s.telephony_db().await;
    Stack::until(
        &t,
        &format!("SELECT status FROM telephony_legs WHERE id = '{leg}'"),
        "ENDED",
        Duration::from_secs(15),
    )
    .await;
    s.wait_call(call, CallState::Cancelled, Duration::from_secs(10))
        .await;

    // Revoke the context racing a new origination: whichever wins, no destination is
    // ever resolved for a revoked context and no Call stays live.
    let revoke_path = format!("/v1/privacy-relay/contexts/{context}/revoke");
    let current = s
        .privacy(
            Method::GET,
            &format!("/v1/privacy-relay/contexts/{context}"),
            None,
        )
        .await
        .json["version"]
        .clone();
    let (call_r, revoke) = tokio::join!(
        s.call_contact(&contact, session),
        s.privacy(
            Method::POST,
            &revoke_path,
            Some(json!({ "expectedVersion": current })),
        )
    );
    assert!(revoke.status.is_success(), "{}", revoke.text);
    if call_r.status == StatusCode::CREATED {
        let call: Uuid = call_r.json["call"]["callId"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        // Enforcement (Privacy Relay -> Calling) ends it; Telephony never connects it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if s.w.state(call).await.0.is_terminal() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "revoked call still live"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let live = Stack::text(
            &t,
            &format!("SELECT count(*)::text FROM telephony_legs WHERE calling_call_id = '{call}' AND status NOT IN ('ENDED', 'FAILED')"),
        )
        .await;
        assert_eq!(live.as_deref(), Some("0"));
    } else {
        assert!(call_r.status.is_client_error(), "{}", call_r.text);
    }
    // After the revocation nothing more can be placed under it.
    let r = s.call_contact(&contact, session).await;
    assert!(r.status.is_client_error(), "{}", r.text);
}
