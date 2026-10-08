//! Shared harness for Privacy Relay integration tests.
//!
//! Requires PostgreSQL (no Docker needed): `TEST_DATABASE_URL` is an admin URL, e.g.
//! `postgres://postgres:postgres@127.0.0.1:5432/postgres`. Each test gets its own
//! database cloned from a freshly migrated template and dropped afterwards. Without
//! `TEST_DATABASE_URL` the tests FAIL unless `CFMEET_SKIP_DB_TESTS=1` is set (then they
//! print an explicit SKIPPED line, never a silent pass).
//!
//! Domain time is a manual clock (no sleeps). Internal requests are signed by real
//! Ed25519 service identities and pass the real authentication middleware.
#![allow(dead_code)]

use std::sync::{Arc, Mutex as StdMutex};

use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
    Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, DelegatedPlatformContext, KeyId, OutboundServiceRequest,
    RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    TokenTtl, TrustPolicy,
};
use cf_service_auth::{
    contact_delegation::CONTACT_AUTHORIZATION, ContactDelegationSigner, ContactDelegationTrust,
    ContactDelegationVerifier, ContactScope,
};
use chrono::{DateTime, Duration, Utc};
use http_body_util::BodyExt;
use privacy_relay::{
    clock::ManualClock,
    config::{FeatureFlag, ServiceAuthRuntime},
    contact_vault::{
        keyring::{generate_keyring_json, FileKeyring, KeyPurpose, KeyVersion},
        ContactVault, VaultKeys,
    },
    enforcement::{CallingEnforcer, DeliveryFailure, EnforcementAck, EnforcementDelivery},
    internal, routes,
    service::RelayService,
    state::AppState,
};
use privacy_relay_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tokio::sync::{Mutex, OnceCell};
use tower::ServiceExt;
use uuid::Uuid;

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
                    "TEST_DATABASE_URL is not set. Privacy Relay integration tests require PostgreSQL \
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

async fn drop_databases_of_dead_runs(conn: &DatabaseConnection) {
    let Ok(rows) = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT datname FROM pg_database WHERE datname LIKE 'prl\\_%'",
        ))
        .await
    else {
        return;
    };
    for row in rows {
        let Ok(name) = row.try_get_by_index::<String>(0) else {
            continue;
        };
        let pid = name.split('_').nth(2).and_then(|p| p.parse::<u32>().ok());
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
                "prl_tmpl_{}_{}",
                std::process::id(),
                Uuid::new_v4().simple()
            );
            let conn = Database::connect(admin).await.expect("connect admin db");
            if std::path::Path::new("/proc/self").exists() {
                drop_databases_of_dead_runs(&conn).await;
            }
            conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
                .await
                .unwrap();
            conn.close().await.ok();
            let tmpl = Database::connect(with_db(admin, &name)).await.unwrap();
            privacy_relay_migration::Migrator::up(&tmpl, None)
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
    pub async fn new() -> Option<Self> {
        let admin = admin_url()?;
        let tmpl = template(&admin).await;
        let name = format!("prl_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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

    pub async fn empty() -> Option<Self> {
        let admin = admin_url()?;
        let name = format!("prl_t_{}_{}", std::process::id(), Uuid::new_v4().simple());
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

/// Fixed start of the manual domain clock.
pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2031-03-01T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub fn at(minutes: i64) -> DateTime<Utc> {
    t0() + Duration::minutes(minutes)
}

pub fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Calling enforcement fake.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct FakeCalling {
    failure: StdMutex<Option<DeliveryFailure>>,
    pub deliveries: StdMutex<Vec<EnforcementDelivery>>,
}

impl FakeCalling {
    pub fn fail_with(&self, failure: Option<DeliveryFailure>) {
        *self.failure.lock().unwrap() = failure;
    }

    pub fn delivered(&self) -> Vec<EnforcementDelivery> {
        self.deliveries.lock().unwrap().clone()
    }

    pub fn delivered_event_ids(&self) -> Vec<String> {
        self.delivered()
            .iter()
            .map(|d| {
                d.payload["enforcementEventId"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl CallingEnforcer for FakeCalling {
    async fn enforce(
        &self,
        delivery: &EnforcementDelivery,
    ) -> Result<EnforcementAck, DeliveryFailure> {
        if let Some(failure) = *self.failure.lock().unwrap() {
            return Err(failure);
        }
        self.deliveries.lock().unwrap().push(delivery.clone());
        Ok(EnforcementAck {
            result: json!({ "status": "APPLIED", "terminated_calls": 0, "matched_calls": 0 }),
        })
    }
}

// ---------------------------------------------------------------------------
// Service identities.
// ---------------------------------------------------------------------------

pub const ALL_CALLING_PERMISSIONS: &[&str] = &[
    "communication.authorize",
    "communication-context.read",
    "communication-context.write",
    "relay-session.read",
    "relay-session.write",
];

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

pub struct Identities {
    pub runtime: ServiceAuthRuntime,
    /// Calling's DEDICATED contact delegation signer (not its service key).
    pub contact_signer: Arc<ContactDelegationSigner>,
    pub contact_jwk: cf_service_auth::PublicJwk,
    /// PKCS#8 PEM of the contact delegation key (tests that mint with another clock).
    pub contact_pem: String,
    pub relay: Arc<ServiceTokenSigner>,
    pub relay_jwk: cf_service_auth::PublicJwk,
    pub calling: Arc<ServiceTokenSigner>,
    pub calling_key: ServiceSigningKey,
    pub telephony: Arc<ServiceTokenSigner>,
    pub untrusted: Arc<ServiceTokenSigner>,
}

pub fn identities(calling_permissions: &[&str]) -> Identities {
    let env = RuntimeEnvironment::Test;
    let clock: cf_service_auth::SharedClock = Arc::new(cf_service_auth::SystemClock);
    let relay_key = ServiceSigningKey::generate(KeyId::parse("privacy-relay-test-1").unwrap());
    let relay_jwk = relay_key.public_jwk();
    let calling_key = ServiceSigningKey::generate(KeyId::parse("calling-test-1").unwrap());
    let telephony_key = ServiceSigningKey::generate(KeyId::parse("telephony-test-1").unwrap());
    let untrusted_key = ServiceSigningKey::generate(KeyId::parse("untrusted-test-1").unwrap());
    let policy = TrustPolicy::from_json(
        &trust_bundle_json(
            cf_service_auth::services::PRIVACY_RELAY,
            env,
            &[
                (
                    cf_service_auth::services::CALLING,
                    vec![calling_key.public_jwk()],
                    calling_permissions,
                ),
                (
                    cf_service_auth::services::TELEPHONY,
                    vec![telephony_key.public_jwk()],
                    &["communication-context.read"],
                ),
            ],
        ),
        &ServiceId::privacy_relay(),
        env,
    )
    .unwrap();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::privacy_relay(),
        env,
        relay_key,
        TokenTtl::default(),
        clock.clone(),
    ));
    let verifier = Arc::new(ServiceTokenVerifier::new(policy, clock.clone()));
    let client = Arc::new(
        InternalServiceClient::new(
            signer.clone(),
            InternalTargets::new(env),
            std::time::Duration::from_secs(5),
        )
        .unwrap(),
    );
    let calling_pem = calling_key.to_pkcs8_pem();
    let calling_copy =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse("calling-test-1").unwrap(), &calling_pem)
            .unwrap();
    let contact_key = ServiceSigningKey::generate(KeyId::parse("calling-contact-test-1").unwrap());
    let contact_jwk = contact_key.public_jwk();
    let contact_pem = contact_key.to_pkcs8_pem().to_string();
    Identities {
        contact_pem,
        contact_signer: Arc::new(
            ContactDelegationSigner::new(
                ServiceId::calling(),
                env,
                contact_key,
                120,
                clock.clone(),
            )
            .unwrap(),
        ),
        contact_jwk,
        runtime: ServiceAuthRuntime::new(signer.clone(), verifier, client).unwrap(),
        relay: signer,
        relay_jwk,
        calling: Arc::new(ServiceTokenSigner::new(
            ServiceId::calling(),
            env,
            calling_copy,
            TokenTtl::default(),
            clock.clone(),
        )),
        calling_key,
        telephony: Arc::new(ServiceTokenSigner::new(
            ServiceId::telephony(),
            env,
            telephony_key,
            TokenTtl::default(),
            clock.clone(),
        )),
        untrusted: Arc::new(ServiceTokenSigner::new(
            ServiceId::parse("cfmeet.numbering").unwrap(),
            env,
            untrusted_key,
            TokenTtl::default(),
            clock,
        )),
    }
}

impl Identities {
    /// Privacy Relay's contact delegation trust: Calling's contact key, every contact scope.
    pub fn contact_verifier(&self) -> ContactDelegationVerifier {
        let trust = ContactDelegationTrust::from_json(
            &contact_trust_json(
                std::slice::from_ref(&self.contact_jwk),
                &[
                    "privacy.contacts.read",
                    "privacy.contacts.write",
                    "privacy.contacts.verify",
                ],
            ),
            &ServiceId::privacy_relay(),
            RuntimeEnvironment::Test,
        )
        .unwrap();
        ContactDelegationVerifier::new(trust, Arc::new(cf_service_auth::SystemClock))
    }
}

pub fn contact_trust_json(keys: &[cf_service_auth::PublicJwk], scopes: &[&str]) -> String {
    json!({
        "bundle_version": 1,
        "token_use": "CONTACT_VAULT_ACCESS",
        "audience": "cfmeet.privacy-relay",
        "runtime_environment": "test",
        "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": keys, "scopes": scopes }],
    })
    .to_string()
}

/// Fresh random encryption + fingerprint keyrings (test-only; never written to disk).
pub fn test_vault_keys(encryption_version: &str, fingerprint_version: &str) -> VaultKeys {
    let ev = KeyVersion::parse(encryption_version).unwrap();
    let fv = KeyVersion::parse(fingerprint_version).unwrap();
    VaultKeys::new(
        Arc::new(
            FileKeyring::from_json(
                &generate_keyring_json(KeyPurpose::Encryption, &ev),
                KeyPurpose::Encryption,
            )
            .unwrap(),
        ),
        Arc::new(
            FileKeyring::from_json(
                &generate_keyring_json(KeyPurpose::Fingerprint, &fv),
                KeyPurpose::Fingerprint,
            )
            .unwrap(),
        ),
    )
    .unwrap()
}

/// A consumer application environment (scope asserted through the delegated context).
#[derive(Debug, Clone)]
pub struct Consumer {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub service_account_id: Uuid,
    pub kind: ApplicationEnvironmentKind,
}

impl Consumer {
    pub fn new(kind: ApplicationEnvironmentKind) -> Self {
        Self {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            service_account_id: Uuid::new_v4(),
            kind,
        }
    }

    pub fn delegated(&self) -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            self.tenant_id,
            self.application_id,
            self.environment_id,
            self.kind,
            self.service_account_id,
        )
    }

    /// Another environment of the same application.
    pub fn sibling_environment(&self) -> Self {
        Self {
            environment_id: Uuid::new_v4(),
            service_account_id: Uuid::new_v4(),
            ..self.clone()
        }
    }
}

// ---------------------------------------------------------------------------
// Harness.
// ---------------------------------------------------------------------------

pub struct Harness {
    pub db: Arc<TestDb>,
    pub clock: ManualClock,
    pub feature: FeatureFlag,
    /// CONTACT_VAULT_ENABLED.
    pub contact_feature: FeatureFlag,
    pub contacts: ContactVault,
    pub vault_keys: VaultKeys,
    /// Calling's contact delegation signer (its own key, wall clock).
    pub contact_signer: Arc<ContactDelegationSigner>,
    pub calling: Arc<FakeCalling>,
    pub relay: RelayService,
    pub state: AppState,
    pub ids: Arc<Identities>,
}

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

    pub fn normalized_error(&self) -> Value {
        let mut v = self.json.clone();
        if let Some(e) = v.get_mut("error") {
            e["requestId"] = Value::Null;
        }
        v
    }
}

impl Harness {
    pub async fn new() -> Option<Self> {
        Self::with_permissions(ALL_CALLING_PERMISSIONS).await
    }

    pub async fn with_permissions(calling_permissions: &[&str]) -> Option<Self> {
        let db = Arc::new(TestDb::new().await?);
        Some(Self::on(db, ManualClock::new(t0()), calling_permissions))
    }

    pub fn on(db: Arc<TestDb>, clock: ManualClock, calling_permissions: &[&str]) -> Self {
        let feature = FeatureFlag::new(true);
        let calling = Arc::new(FakeCalling::default());
        let ids = Arc::new(identities(calling_permissions));
        let relay = RelayService::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            feature.clone(),
            RuntimeEnvironment::Test,
            calling.clone(),
        );
        let contact_feature = FeatureFlag::new(true);
        let vault_keys = test_vault_keys("ck-test-1", "fk-test-1");
        let contacts = ContactVault::new(
            db.conn.clone(),
            Arc::new(clock.clone()),
            contact_feature.clone(),
            Some(vault_keys.clone()),
        );
        let state = AppState::new(relay.clone(), ids.runtime.clone())
            .with_contact_vault(contacts.clone(), Some(ids.contact_verifier()));
        Self {
            db,
            clock,
            feature,
            contact_feature,
            contacts,
            vault_keys,
            contact_signer: ids.contact_signer.clone(),
            calling,
            relay,
            state,
            ids,
        }
    }

    /// Re-deploys the Contact Vault with `keys` (key rotation steps).
    pub fn set_vault_keys(&mut self, keys: VaultKeys) {
        self.contacts = self.vault_with(Some(keys.clone()));
        self.vault_keys = keys;
        self.state = self
            .state
            .clone()
            .with_contact_vault(self.contacts.clone(), Some(self.ids.contact_verifier()));
    }

    /// Another Contact Vault instance over the same database / clock / flag.
    pub fn vault_with(&self, keys: Option<VaultKeys>) -> ContactVault {
        ContactVault::new(
            self.db.conn.clone(),
            Arc::new(self.clock.clone()),
            self.contact_feature.clone(),
            keys,
        )
    }

    /// A second instance over the same database and clock (another worker).
    pub fn sibling(&self) -> RelayService {
        RelayService::new(
            self.db.conn.clone(),
            Arc::new(self.clock.clone()),
            self.feature.clone(),
            RuntimeEnvironment::Test,
            self.calling.clone(),
        )
    }

    pub fn conn(&self) -> &DatabaseConnection {
        &self.db.conn
    }

    pub fn router(&self) -> Router {
        routes::app(self.state.clone(), internal::routes())
    }

    /// A request signed by `signer` with `delegated` (None: no delegated context).
    pub async fn send_as(
        &self,
        signer: &ServiceTokenSigner,
        method: Method,
        path: &str,
        delegated: Option<&Consumer>,
        body: Option<Value>,
    ) -> Response {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::privacy_relay());
        if let Some(c) = delegated {
            outbound = outbound.with_delegated_context(c.delegated());
        }
        let signed = signer.sign(&outbound).unwrap();
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in signed.headers().iter() {
            builder = builder.header(name, value);
        }
        self.raw(builder, body).await
    }

    pub async fn raw(
        &self,
        builder: axum::http::request::Builder,
        body: Option<Value>,
    ) -> Response {
        let request = match body {
            Some(b) => builder
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.router().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        Response {
            status,
            headers,
            json,
            text,
        }
    }

    /// Calling acting for `consumer`.
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        consumer: &Consumer,
        body: Option<Value>,
    ) -> Response {
        let calling = self.ids.calling.clone();
        self.send_as(&calling, method, path, Some(consumer), body)
            .await
    }

    pub async fn post(&self, path: &str, consumer: &Consumer, body: Value) -> Response {
        self.call(Method::POST, path, consumer, Some(body)).await
    }

    pub async fn get(&self, path: &str, consumer: &Consumer) -> Response {
        self.call(Method::GET, path, consumer, None).await
    }

    /// A command with a fresh idempotency key.
    pub async fn command(&self, path: &str, consumer: &Consumer, request: Value) -> Response {
        self.keyed(path, consumer, &Uuid::new_v4().to_string(), request)
            .await
    }

    pub async fn keyed(
        &self,
        path: &str,
        consumer: &Consumer,
        key: &str,
        request: Value,
    ) -> Response {
        self.post(
            path,
            consumer,
            json!({ "idempotencyKey": key, "request": request }),
        )
        .await
    }

    // --- Contact Vault helpers -------------------------------------------------

    /// A Contact Vault access token for `consumer` (Calling's contact delegation key).
    pub fn contact_token(&self, consumer: &Consumer, scopes: &[ContactScope]) -> String {
        self.contact_signer
            .sign(&ServiceId::privacy_relay(), &consumer.delegated(), scopes)
            .unwrap()
            .expose_for_delivery()
            .to_string()
    }

    pub fn writer(&self, consumer: &Consumer) -> String {
        self.contact_token(consumer, &[ContactScope::Read, ContactScope::Write])
    }

    /// A contact route request with `X-CF-Contact-Authorization: Bearer <token>`.
    pub async fn contact_call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            builder = builder.header(CONTACT_AUTHORIZATION, format!("Bearer {t}"));
        }
        self.raw(builder, body).await
    }

    /// Protects `number` for `consumer` and returns the safe contact.
    pub async fn create_contact(&self, consumer: &Consumer, number: &str) -> Value {
        let token = self.writer(consumer);
        let r = self
            .contact_call(
                Method::POST,
                "/v1/privacy-relay/contacts",
                Some(&token),
                Some(json!({ "telephoneNumber": number })),
            )
            .await;
        assert!(
            r.status == StatusCode::CREATED || r.status == StatusCode::OK,
            "{}",
            r.text
        );
        r.json["contact"].clone()
    }

    // --- domain helpers ------------------------------------------------------

    pub async fn create_context(&self, consumer: &Consumer, request: Value) -> Value {
        let r = self
            .command("/internal/v1/contexts", consumer, request)
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["context"].clone()
    }

    /// A bidirectional VOICE context between `subjects` valid t0 .. t0+60min.
    pub async fn bidirectional(&self, consumer: &Consumer, subjects: &[Uuid]) -> Value {
        self.create_context(consumer, context_request(subjects, "BIDIRECTIONAL", &[]))
            .await
    }

    /// A bidirectional context starting at `minute` (60 minutes long).
    pub async fn bidirectional_at(
        &self,
        consumer: &Consumer,
        subjects: &[Uuid],
        minute: i64,
    ) -> Value {
        let mut req = context_request(subjects, "BIDIRECTIONAL", &[]);
        req["startsAt"] = json!(iso(at(minute)));
        req["expiresAt"] = json!(iso(at(minute + 60)));
        self.create_context(consumer, req).await
    }

    pub async fn create_session(
        &self,
        consumer: &Consumer,
        context_id: &str,
        request: Value,
    ) -> Value {
        let r = self
            .command(
                &format!("/internal/v1/contexts/{context_id}/sessions"),
                consumer,
                request,
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["session"].clone()
    }

    pub async fn app_session(&self, consumer: &Consumer, context_id: &str) -> Value {
        self.create_session(consumer, context_id, json!({ "relayStrategy": "APP_ONLY" }))
            .await
    }

    pub async fn authorize(
        &self,
        consumer: &Consumer,
        session_id: &str,
        from: Uuid,
        to: &[Uuid],
    ) -> Response {
        self.post(
            "/internal/v1/communication/authorize",
            consumer,
            json!({
                "relaySessionId": session_id,
                "source": { "principalId": from },
                "targets": to.iter().map(|t| json!({ "principalId": t })).collect::<Vec<_>>(),
                "channel": "VOICE",
                "route": "APPLICATION",
                "purpose": "CALL_CREATE",
            }),
        )
        .await
    }

    /// The decision reason of `from -> to` (asserting a 200 decision).
    pub async fn reason(
        &self,
        consumer: &Consumer,
        session_id: &str,
        from: Uuid,
        to: &[Uuid],
    ) -> String {
        let r = self.authorize(consumer, session_id, from, to).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        r.json["reason"].as_str().unwrap().to_string()
    }

    pub async fn version_of(&self, consumer: &Consumer, context_id: &str) -> i64 {
        self.get(&format!("/internal/v1/contexts/{context_id}"), consumer)
            .await
            .json["version"]
            .as_i64()
            .unwrap()
    }

    pub async fn lifecycle(
        &self,
        consumer: &Consumer,
        context_id: &str,
        action: &str,
        expected: i64,
    ) -> Response {
        self.command(
            &format!("/internal/v1/contexts/{context_id}/{action}"),
            consumer,
            json!({ "expectedVersion": expected }),
        )
        .await
    }

    pub async fn count(&self, sql: &str) -> i64 {
        scalar(self.conn(), sql).await
    }
}

pub fn context_request(subjects: &[Uuid], policy: &str, directions: &[(Uuid, Uuid)]) -> Value {
    let mut v = json!({
        "externalContextReference": format!("ref-{}", Uuid::new_v4().simple()),
        "participants": subjects.iter().map(|s| json!({ "principalId": s })).collect::<Vec<_>>(),
        "allowedChannels": ["VOICE"],
        "directionPolicy": policy,
        "startsAt": iso(at(0)),
        "expiresAt": iso(at(60)),
    });
    if !directions.is_empty() {
        v["directions"] = json!(directions
            .iter()
            .map(|(f, t)| json!({ "from": { "principalId": f }, "to": { "principalId": t } }))
            .collect::<Vec<_>>());
    }
    v
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

pub async fn exec(conn: &DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    conn.execute_unprepared(sql).await.map(|_| ())
}

/// Every row of every table, as text.
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
        for row in conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT t::text FROM {table} t"),
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

/// Every row of one table, as text.
pub async fn dump_table(conn: &DatabaseConnection, table: &str) -> String {
    let mut out = String::new();
    for row in conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT t::text FROM {table} t"),
        ))
        .await
        .unwrap()
    {
        out.push_str(&row.try_get_by_index::<String>(0).unwrap());
        out.push('\n');
    }
    out
}

/// Rows of `sql`, in select order; every column must be text (cast with `::text`).
pub async fn rows(conn: &DatabaseConnection, sql: &str) -> Vec<Vec<String>> {
    conn.query_all(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|r| {
        let mut out = Vec::new();
        while let Ok(v) = r.try_get_by_index::<Option<String>>(out.len()) {
            out.push(v.unwrap_or_default());
        }
        out
    })
    .collect()
}

pub fn principals(n: usize) -> Vec<Uuid> {
    (0..n).map(|_| Uuid::new_v4()).collect()
}
