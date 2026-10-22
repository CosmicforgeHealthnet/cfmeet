//! Part V K §§151–154 — cross-service certification across REAL service processes.
//!
//! Closes the three mandatory scenarios Parts H–J left at PARTIAL (each side had only been
//! tested against a scripted, signed peer):
//!
//! * Scenario 4 — Privacy Relay revocation: the REAL Privacy Relay binary pushes
//!   communication-context authority (its database-enqueued jobs, its worker) into the
//!   REAL Messaging binary; conversations lock, unlock and close; Messaging outages are
//!   retried; nothing is granted while authority is missing.
//! * Scenario 5 — Managed SMS Bridge: the REAL Messaging binary and the REAL SMS Gateway
//!   binary with its deterministic synthetic carrier (no real carrier, no paid traffic).
//! * Scenario 9 — Webhook privacy: the REAL Messaging binary relays ids-only events to the
//!   REAL Calling binary, which delivers SIGNED webhooks to a customer endpoint (a local
//!   receiver in this test) with retries; no content, ciphertext, key or private
//!   identifier crosses.
//!
//! Every service has its OWN database migrated by ITS OWN migrations, and they talk only
//! over signed internal HTTP (`cf-service-auth`, receiver-authoritative trust bundles with
//! exactly the permissions each integration needs). The test reads each database only as
//! evidence, never on behalf of a service.
//!
//! Requires `TEST_DATABASE_URL`, `PLATFORM_ACCESS_BIN`, `PLATFORM_ACCESS_MIGRATION_BIN` and
//! per journey `PRIVACY_RELAY_BIN` / `PRIVACY_RELAY_MIGRATION_BIN`, `SMS_GATEWAY_BIN` /
//! `SMS_GATEWAY_MIGRATION_BIN`, `CALLING_BIN` / `CALLING_MIGRATION_BIN`. Without them a
//! journey is SKIPPED with an explicit line, unless `CROSS_SERVICE_E2E_REQUIRED=1` (CI),
//! where it FAILS instead. `CROSS_SERVICE_E2E_LOGS=1` shows the services' own logs.

mod common;

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, KeyId, OutboundServiceRequest, PublicJwk,
    RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl,
};
use common::{gate::TcpGate, with_db, TestDb};
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

thread_local! {
    /// The runtime environment of the journey on this test thread (`test` by default;
    /// the webhook journey runs as `local`, the only runtime in which Calling accepts a
    /// loopback webhook receiver).
    static RUNTIME: std::cell::Cell<RuntimeEnvironment> =
        const { std::cell::Cell::new(RuntimeEnvironment::Test) };
}

fn env_rt() -> RuntimeEnvironment {
    RUNTIME.with(|r| r.get())
}

fn env_name() -> String {
    match env_rt() {
        RuntimeEnvironment::Local => "local".into(),
        _ => "test".into(),
    }
}
/// Deterministic test-only pepper of the test Platform Access (never used outside tests).
const TEST_PEPPER: &str = "messaging-xs-e2e-test-pepper-0b7e41c9a2d35f86";
pub const MESSAGING_SCOPES: [&str; 4] = [
    "messages.send",
    "messages.read",
    "conversations.create",
    "conversations.manage",
];

fn required() -> bool {
    std::env::var("CROSS_SERVICE_E2E_REQUIRED").as_deref() == Ok("1")
}

fn bins(names: &[&str]) -> Option<Vec<PathBuf>> {
    let found: Vec<Option<PathBuf>> = names
        .iter()
        .map(|k| std::env::var_os(k).map(PathBuf::from))
        .collect();
    if found.iter().all(Option::is_some) {
        return Some(found.into_iter().map(Option::unwrap).collect());
    }
    let missing: Vec<&str> = names
        .iter()
        .zip(&found)
        .filter(|(_, f)| f.is_none())
        .map(|(n, _)| *n)
        .collect();
    if required() {
        panic!("CROSS_SERVICE_E2E_REQUIRED=1 but {missing:?} are not set");
    }
    eprintln!("SKIPPED: cross-service journey ({missing:?} not set)");
    None
}

fn logs() -> Stdio {
    if std::env::var("CROSS_SERVICE_E2E_LOGS").as_deref() == Ok("1") {
        Stdio::inherit()
    } else {
        Stdio::null()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_ready(
    http: &reqwest::Client,
    url: &str,
    ready: &str,
    child: &mut tokio::process::Child,
) -> bool {
    for _ in 0..600 {
        if let Ok(Some(_)) = child.try_wait() {
            return false;
        }
        if let Ok(r) = http.get(format!("{url}{ready}")).send().await {
            // Only if the answer came from THIS child (not another test's process that
            // took the probed port first).
            if r.status().is_success() {
                return child
                    .id()
                    .is_some_and(|pid| common::process_owns_listener(pid, common::url_port(url)));
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

pub async fn create_db(admin: &str, prefix: &str) -> String {
    let name = format!("{prefix}_{}", Uuid::new_v4().simple());
    let conn = Database::connect(admin).await.unwrap();
    conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    conn.close().await.ok();
    with_db(admin, &name)
}

pub fn migrate(bin: &Path, url: &str) {
    let out = std::process::Command::new(bin)
        .arg("up")
        .env("DATABASE_URL", url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}: {}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .unwrap_or_else(|| panic!("no row: {sql}"))
    .try_get_by_index::<i64>(0)
    .unwrap()
}

pub async fn text(db: &DatabaseConnection, sql: &str) -> Option<String> {
    db.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
}

/// Polls `f` until it returns `Some` (at most `secs` seconds).
pub async fn eventually<T, F, Fut>(secs: u64, what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out after {secs}s waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub fn trust_bundle(receiver: &str, grants: &[(&str, &PublicJwk, &[&str])]) -> String {
    json!({
        "bundle_version": 1, "receiver": receiver, "runtime_environment": env_rt(),
        "trusted_issuers": grants.iter().map(|(issuer, key, permissions)| json!({
            "issuer": issuer, "audience": receiver, "runtime_environment": env_rt(),
            "keys": [key], "permissions": permissions,
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// A synthetic tenant / application / SANDBOX environment / service account.
#[derive(Clone, Copy, Debug)]
pub struct Tenant {
    pub tenant: Uuid,
    pub app: Uuid,
    pub env: Uuid,
    pub sa: Uuid,
}

impl Tenant {
    pub fn delegated(&self) -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            self.tenant,
            self.app,
            self.env,
            ApplicationEnvironmentKind::Sandbox,
            self.sa,
        )
    }
}

/// A running service process (killed on drop).
pub struct Proc {
    pub child: Option<tokio::process::Child>,
    pub url: String,
    pub port: u16,
}

impl Proc {
    pub async fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            c.kill().await.ok();
            let _ = c.wait().await;
        }
    }
}

/// Spawns `bin` with exactly `env` (+ PATH) on a free port (`bind_var`), or on `port`.
pub async fn spawn(
    http: &reqwest::Client,
    bin: &Path,
    ready: &str,
    bind_var: &str,
    env: &[(String, String)],
    port: Option<u16>,
) -> Proc {
    for _ in 0..if port.is_some() { 50 } else { 5 } {
        let p = port.unwrap_or_else(free_port);
        let mut c = tokio::process::Command::new(bin);
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env(bind_var, format!("127.0.0.1:{p}"))
            .stdout(Stdio::null())
            .stderr(logs())
            .kill_on_drop(true);
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let url = format!("http://127.0.0.1:{p}");
        if wait_ready(http, &url, ready, &mut child).await {
            return Proc {
                child: Some(child),
                url,
                port: p,
            };
        }
        let _ = child.kill().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} did not become ready", bin.display());
}

pub fn key(dir: &Path, file: &str, kid: &str) -> ServiceSigningKey {
    let k = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    std::fs::write(dir.join(file), k.to_pkcs8_pem().as_bytes()).unwrap();
    k
}

pub fn signer(id: ServiceId, key: ServiceSigningKey) -> Arc<ServiceTokenSigner> {
    Arc::new(ServiceTokenSigner::new(
        id,
        env_rt(),
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ))
}

pub struct Response {
    pub status: u16,
    pub json: Value,
    pub text: String,
}

impl Response {
    pub fn reason(&self) -> &str {
        self.json["error"]["reason"]
            .as_str()
            .or(self.json["error"]["code"].as_str())
            .unwrap_or("")
    }
}

pub async fn send(r: reqwest::RequestBuilder) -> Response {
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Response {
        status,
        json: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

/// Signs a request from `signer` to `audience` (optionally with a delegated scope).
pub fn signed(
    http: &reqwest::Client,
    signer: &ServiceTokenSigner,
    audience: ServiceId,
    delegated: Option<DelegatedPlatformContext>,
    method: reqwest::Method,
    url: String,
) -> reqwest::RequestBuilder {
    let mut req = OutboundServiceRequest::new_operation(audience);
    if let Some(d) = delegated {
        req = req.with_delegated_context(d);
    }
    let s = signer.sign(&req).unwrap();
    let mut r = http.request(method, url);
    for (k, v) in s.headers().iter() {
        r = r.header(k.as_str(), v.as_bytes());
    }
    r
}

/// Real Platform Access + a real Messaging process (each its own database).
pub struct Stack {
    pub admin: String,
    pub dir: PathBuf,
    pub http: reqwest::Client,
    pub pa: Proc,
    pub operator: Arc<ServiceTokenSigner>,
    pub messaging_key: ServiceSigningKey,
    pub messaging_env: Vec<(String, String)>,
    pub messaging: Option<Proc>,
    pub messaging_db: Option<TestDb>,
}

impl Stack {
    /// Platform Access running; Messaging configured (not started — callers add peers).
    pub async fn new(extra_pa_grants: &[(&str, &PublicJwk, &[&str])]) -> Option<Stack> {
        let admin = common::admin_url()?;
        let b = bins(&["PLATFORM_ACCESS_BIN", "PLATFORM_ACCESS_MIGRATION_BIN"])?;
        let dir = std::env::temp_dir().join(format!("cf-xs-e2e-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let pa_db = create_db(&admin, "xs_pa").await;
        migrate(&b[1], &pa_db);
        key(&dir, "pa-key.pem", "pa-xs-1");
        let messaging_key = key(&dir, "messaging-key.pem", "messaging-xs-1");
        let operator_key = key(&dir, "operator-key.pem", "operator-xs-1");
        key(&dir, "messaging-ct.pem", "messaging-ct-xs");
        key(&dir, "messaging-kt.pem", "messaging-kt-xs");
        let ring = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            rand::random::<[u8; 32]>(),
        );
        std::fs::write(dir.join("managed.ring"), format!("xs-1 {ring}\n")).unwrap();
        use cf_service_auth::services as s;
        let mjwk = messaging_key.public_jwk();
        let ojwk = operator_key.public_jwk();
        let mut grants: Vec<(&str, &PublicJwk, &[&str])> = vec![
            (
                s::MESSAGING,
                &mjwk,
                &[
                    "platform-credential.authenticate",
                    "platform-principal.resolve",
                    "platform-identity.read",
                ],
            ),
            (
                s::OPERATOR_CONSOLE,
                &ojwk,
                &["platform-identity.administer"],
            ),
        ];
        grants.extend_from_slice(extra_pa_grants);
        std::fs::write(
            dir.join("pa-trust.json"),
            trust_bundle(s::PLATFORM_ACCESS, &grants),
        )
        .unwrap();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let p = |n: &str| dir.join(n).display().to_string();
        let pa_env: Vec<(String, String)> = [
            ("DATABASE_URL", pa_db.clone()),
            ("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "4".into()),
            ("API_CREDENTIAL_PEPPER", TEST_PEPPER.into()),
            ("CF_RUNTIME_ENVIRONMENT", env_name()),
            ("CF_SERVICE_AUTH_KEY_ID", "pa-xs-1".into()),
            ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", p("pa-key.pem")),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", p("pa-trust.json")),
            ("RUST_LOG", "platform_access=error".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let pa = spawn(
            &http,
            &b[0],
            "/ready",
            "PLATFORM_ACCESS_BIND_ADDRESS",
            &pa_env,
            None,
        )
        .await;
        let messaging_env: Vec<(String, String)> = [
            ("CF_RUNTIME_ENVIRONMENT", env_name()),
            ("RUST_LOG", "messaging=warn".into()),
            ("CF_SERVICE_AUTH_KEY_ID", "messaging-xs-1".into()),
            ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", p("messaging-key.pem")),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", p("messaging-trust.json")),
            ("CF_PLATFORM_ACCESS_BASE_URL", pa.url.clone()),
            ("MESSAGING_AUTH_CACHE_TTL_SECONDS", "30".into()),
            ("MESSAGING_IDENTITY_FEED_INTERVAL_MS", "250".into()),
            ("MESSAGING_WORKER_INTERVAL_MS", "200".into()),
            ("MESSAGING_KT_LOG_ID", "cfmeet-messaging:xs-e2e".into()),
            ("MESSAGING_KT_SIGNING_KEY_FILE", p("messaging-kt.pem")),
            (
                "MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE",
                p("messaging-ct.pem"),
            ),
            ("MESSAGING_MANAGED_KEYRING_FILE", p("managed.ring")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        Some(Stack {
            admin,
            dir,
            http,
            pa,
            operator: signer(ServiceId::operator_console(), operator_key),
            messaging_key,
            messaging_env,
            messaging: None,
            messaging_db: None,
        })
    }

    pub fn path(&self, n: &str) -> String {
        self.dir.join(n).display().to_string()
    }

    /// Messaging's trust bundle: exactly the peers a journey grants.
    pub fn messaging_trust(&self, grants: &[(&str, &PublicJwk, &[&str])]) {
        std::fs::write(
            self.dir.join("messaging-trust.json"),
            trust_bundle(cf_service_auth::services::MESSAGING, grants),
        )
        .unwrap();
    }

    pub async fn start_messaging(&mut self, extra: &[(&str, String)]) {
        if self.messaging_db.is_none() {
            let db = TestDb::empty().await.unwrap();
            messaging_migration::Migrator::up(&db.conn, None)
                .await
                .unwrap();
            self.messaging_db = Some(db);
        }
        let mut env = self.messaging_env.clone();
        env.push((
            "DATABASE_URL".into(),
            self.messaging_db.as_ref().unwrap().url.clone(),
        ));
        for (k, v) in extra {
            env.retain(|(e, _)| e != k);
            env.push((k.to_string(), v.clone()));
        }
        self.messaging_env = env.clone();
        let port = self.messaging.as_ref().map(|m| m.port);
        let bin = PathBuf::from(env!("CARGO_BIN_EXE_messaging"));
        let proc = spawn(
            &self.http,
            &bin,
            "/ready",
            "MESSAGING_BIND_ADDRESS",
            &env,
            port,
        )
        .await;
        self.messaging = Some(proc);
    }

    /// Stops Messaging; `start_messaging(&[])` restarts it on the SAME port.
    pub async fn stop_messaging(&mut self) {
        if let Some(m) = self.messaging.as_mut() {
            m.stop().await;
        }
    }

    pub fn m_url(&self) -> String {
        self.messaging.as_ref().unwrap().url.clone()
    }

    pub fn mdb(&self) -> &DatabaseConnection {
        &self.messaging_db.as_ref().unwrap().conn
    }

    async fn admin_call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        let mut r = signed(
            &self.http,
            &self.operator,
            ServiceId::platform_access(),
            None,
            method,
            format!("{}{path}", self.pa.url),
        )
        .header("x-cf-operator", "messaging-cross-service-certification");
        if let Some(b) = body {
            r = r.json(&b);
        }
        send(r).await
    }

    pub async fn created(&self, path: &str, body: Value) -> Value {
        let r = self
            .admin_call(reqwest::Method::POST, path, Some(body))
            .await;
        assert!(
            (200..300).contains(&r.status),
            "POST {path}: {} {}",
            r.status,
            r.text
        );
        r.json
    }

    /// A synthetic tenant of `classification` whose application is allowed `granted`.
    pub async fn tenant(&self, classification: &str, granted: &[&str]) -> Tenant {
        let slug = format!("t{}", &Uuid::new_v4().simple().to_string()[..12]);
        let t = self
            .created(
                "/internal/v1/admin/tenants",
                json!({ "slug": slug, "displayName": "cross-service certification", "classification": classification }),
            )
            .await;
        let tenant = Uuid::parse_str(t["id"].as_str().unwrap()).unwrap();
        let a = self
            .created(
                &format!("/internal/v1/admin/tenants/{tenant}/applications"),
                json!({ "slug": "app", "displayName": "app" }),
            )
            .await;
        let app = Uuid::parse_str(a["id"].as_str().unwrap()).unwrap();
        for s in granted {
            self.created(
                &format!("/internal/v1/admin/applications/{app}/scopes"),
                json!({ "scope": s }),
            )
            .await;
        }
        let e = self
            .created(
                &format!("/internal/v1/admin/applications/{app}/environments"),
                json!({ "kind": "SANDBOX" }),
            )
            .await;
        let env = Uuid::parse_str(e["id"].as_str().unwrap()).unwrap();
        let sa = self
            .created(
                &format!("/internal/v1/admin/environments/{env}/service-accounts"),
                json!({ "name": "backend" }),
            )
            .await;
        Tenant {
            tenant,
            app,
            env,
            sa: Uuid::parse_str(sa["id"].as_str().unwrap()).unwrap(),
        }
    }

    pub async fn credential(&self, t: &Tenant, scopes: &[&str]) -> String {
        let c = self
            .created(
                &format!("/internal/v1/admin/service-accounts/{}/credentials", t.sa),
                json!({ "scopes": scopes }),
            )
            .await;
        c["secret"].as_str().unwrap().to_string()
    }

    pub async fn principal(&self, t: &Tenant, kind: &str) -> Uuid {
        let p = self
            .created(
                &format!("/internal/v1/admin/environments/{}/principals", t.env),
                json!({ "kind": kind, "externalReference": format!("p-{}", Uuid::new_v4().simple()) }),
            )
            .await;
        Uuid::parse_str(p["principal"]["id"].as_str().unwrap()).unwrap()
    }

    /// A customer call to Messaging's public API.
    pub async fn customer(
        &self,
        token: &str,
        acting: Option<Uuid>,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        let mut r = self
            .http
            .request(method.clone(), format!("{}{path}", self.m_url()))
            .bearer_auth(token);
        if method == reqwest::Method::POST {
            r = r.header("idempotency-key", Uuid::new_v4().to_string());
        }
        if let Some(a) = acting {
            r = r.header("cf-acting-principal", a.to_string());
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        send(r).await
    }

    /// Allows MANAGED for `class` in the token's environment policy.
    pub async fn allow_managed(&self, token: &str, class: &str) {
        let cur = self
            .customer(
                token,
                None,
                reqwest::Method::GET,
                "/v1/messaging/policy",
                None,
            )
            .await;
        assert_eq!(cur.status, 200, "{}", cur.text);
        let r = self
            .customer(
                token,
                None,
                reqwest::Method::PUT,
                "/v1/messaging/policy",
                Some(json!({ "expectedVersion": cur.json["version"], "policy": { "contexts": {
                    class: { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } } })),
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
    }

    pub async fn add_member(&self, token: &str, owner: Uuid, cid: &str, p: Uuid) {
        let r = self
            .customer(
                token,
                Some(owner),
                reqwest::Method::POST,
                &format!("/v1/conversations/{cid}/members"),
                Some(json!({ "principalId": p })),
            )
            .await;
        assert!(r.status == 200 || r.status == 201, "{}", r.text);
    }

    /// A real OpenMLS client device (keys generated here, in the "client").
    pub async fn device(
        &self,
        token: &str,
        principal: Uuid,
    ) -> cf_messaging_crypto::device::Device {
        use reqwest::Method as M;
        let ch = self
            .customer(
                token,
                Some(principal),
                M::POST,
                "/v1/messaging/devices/registration-challenges",
                Some(json!({})),
            )
            .await;
        assert_eq!(ch.status, 201, "{}", ch.text);
        let ch = ch.json;
        let device = cf_messaging_crypto::device::Device::generate(
            Uuid::parse_str(ch["deviceId"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let nonce: [u8; 32] = hex::decode(ch["nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let kp = device
            .registration_key_package(
                Uuid::parse_str(ch["challengeId"].as_str().unwrap()).unwrap(),
                &nonce,
            )
            .unwrap();
        let d = self
            .customer(
                token,
                Some(principal),
                M::POST,
                "/v1/messaging/devices",
                Some(json!({ "challengeId": ch["challengeId"], "keyPackage": common::b64(&kp) })),
            )
            .await;
        assert_eq!(d.status, 201, "{}", d.text);
        let r = self
            .customer(
                token,
                Some(principal),
                M::POST,
                &format!("/v1/messaging/devices/{}/key-packages", device.device_id()),
                Some(json!({ "keyPackages": [common::b64(&device.key_package().unwrap())] })),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.text);
        device
    }

    pub async fn send_text(&self, token: &str, from: Uuid, cid: &str, text: &str) -> Response {
        self.customer(
            token,
            Some(from),
            reqwest::Method::POST,
            &format!("/v1/conversations/{cid}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": text })),
        )
        .await
    }
}

fn now_plus(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// =====================================================================================
// Scenario 4 — Privacy Relay revocation (real Privacy Relay + real Messaging)
// =====================================================================================

#[tokio::test]
async fn scenario_4_privacy_relay_authority_reaches_real_messaging() {
    let Some(b) = bins(&["PRIVACY_RELAY_BIN", "PRIVACY_RELAY_MIGRATION_BIN"]) else {
        return;
    };
    let Some(mut st) = Stack::new(&[]).await else {
        return;
    };
    use cf_service_auth::services as s;
    use reqwest::Method as M;
    let relay_key = key(&st.dir, "relay-key.pem", "privacy-relay-xs-1");
    // The test plays Calling (the consumer of Privacy Relay's context API).
    let calling_key = key(&st.dir, "calling-key.pem", "calling-xs-1");
    let rjwk = relay_key.public_jwk();
    let cjwk = calling_key.public_jwk();
    // Messaging grants Privacy Relay EXACTLY messaging-context-authority.apply.
    st.messaging_trust(&[(
        s::PRIVACY_RELAY,
        &rjwk,
        &["messaging-context-authority.apply"],
    )]);
    st.start_messaging(&[]).await;
    std::fs::write(
        st.dir.join("relay-trust.json"),
        trust_bundle(
            s::PRIVACY_RELAY,
            &[(
                s::CALLING,
                &cjwk,
                &[
                    "communication.authorize",
                    "communication-context.read",
                    "communication-context.write",
                    "relay-session.read",
                    "relay-session.write",
                ],
            )],
        ),
    )
    .unwrap();
    let relay_db_url = create_db(&st.admin, "xs_relay").await;
    migrate(&b[1], &relay_db_url);
    let relay_db = Database::connect(&relay_db_url).await.unwrap();
    let relay_env: Vec<(String, String)> = [
        ("DATABASE_URL", relay_db_url.clone()),
        ("PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS", "4".into()),
        (
            "PRIVACY_RELAY_METRICS_ADDRESS",
            format!("127.0.0.1:{}", free_port()),
        ),
        ("PRIVACY_RELAY_ENABLED", "true".into()),
        ("PRIVACY_RELAY_WORKER_INTERVAL_MS", "200".into()),
        ("CF_RUNTIME_ENVIRONMENT", env_name()),
        ("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-xs-1".into()),
        ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", st.path("relay-key.pem")),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", st.path("relay-trust.json")),
        ("MESSAGING_INTERNAL_BASE_URL", st.m_url()),
        ("RUST_LOG", "privacy_relay=warn".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let relay = spawn(
        &st.http,
        &b[0],
        "/health",
        "PRIVACY_RELAY_BIND_ADDRESS",
        &relay_env,
        None,
    )
    .await;
    let calling = signer(ServiceId::calling(), calling_key);

    // An EXTERNAL tenant with a backend credential and two principals.
    let t = st.tenant("EXTERNAL", &MESSAGING_SCOPES).await;
    let token = st.credential(&t, &MESSAGING_SCOPES).await;
    let alice = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let bob = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    st.allow_managed(&token, "GROUP").await;

    let http = st.http.clone();
    let m_url = st.m_url();
    let mdb = st.mdb().clone();
    let relay_call = |path: String, body: Value| {
        signed(
            &http,
            &calling,
            ServiceId::privacy_relay(),
            Some(t.delegated()),
            M::POST,
            format!("{}{path}", relay.url),
        )
        .json(&body)
    };
    let create_context = || async {
        let r = send(relay_call(
            "/internal/v1/contexts".into(),
            json!({ "idempotencyKey": Uuid::new_v4(), "request": {
                "externalContextReference": format!("ref-{}", Uuid::new_v4().simple()),
                "participants": [{ "principalId": alice }, { "principalId": bob }],
                "allowedChannels": ["VOICE", "MESSAGING"],
                "directionPolicy": "BIDIRECTIONAL",
                "startsAt": now_plus(-1), "expiresAt": now_plus(60) } }),
        ))
        .await;
        assert_eq!(r.status, 201, "{}", r.text);
        r.json["context"]["contextId"].as_str().unwrap().to_string()
    };
    let lifecycle = |id: String, action: &'static str, expected: i64| {
        let fut = send(relay_call(
            format!("/internal/v1/contexts/{id}/{action}"),
            json!({ "idempotencyKey": Uuid::new_v4(), "request": { "expectedVersion": expected } }),
        ));
        async move {
            let r = fut.await;
            assert_eq!(r.status, 200, "{action}: {}", r.text);
        }
    };
    let create_conv = |reference: String| {
        let r = http
            .post(format!("{m_url}/v1/conversations"))
            .bearer_auth(&token)
            .header("cf-acting-principal", alice.to_string())
            .header("idempotency-key", Uuid::new_v4().to_string())
            .json(
                &json!({ "contextClass": "GROUP", "securityProfile": "MANAGED",
                "context": { "namespace": "privacy-relay", "reference": reference } }),
            );
        send(r)
    };
    let lifecycle_state = |cid: String| {
        let db = mdb.clone();
        async move {
            text(
                &db,
                &format!("SELECT lifecycle_state FROM messaging_conversations WHERE id = '{cid}'"),
            )
            .await
            .unwrap()
        }
    };

    // Before Privacy Relay speaks, nothing can claim an unknown context.
    let unknown = create_conv(Uuid::new_v4().to_string()).await;
    assert_eq!(
        unknown.reason(),
        "CONTEXT_AUTHORITY_UNKNOWN",
        "{}",
        unknown.text
    );

    // 1. ACTIVE authority propagates through Privacy Relay's own worker.
    let started = std::time::Instant::now();
    let ctx = create_context().await;
    let conv = eventually(20, "ACTIVE authority applied in Messaging", || async {
        let r = create_conv(ctx.clone()).await;
        (r.status == 201).then_some(r)
    })
    .await;
    println!(
        "EVIDENCE scenario4 activation_propagation_ms={}",
        started.elapsed().as_millis()
    );
    let cid = conv.json["conversationId"].as_str().unwrap().to_string();
    st.add_member(&token, alice, &cid, bob).await;
    let r = st
        .send_text(&token, alice, &cid, "relay-protected hello")
        .await;
    assert_eq!(r.status, 201, "{}", r.text);
    // Another tenant can never see or claim the context (environment-scoped authority).
    let other = st.tenant("EXTERNAL", &MESSAGING_SCOPES).await;
    let other_token = st.credential(&other, &MESSAGING_SCOPES).await;
    let carol = st.principal(&other, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    st.allow_managed(&other_token, "GROUP").await;
    let r = st
        .customer(
            &other_token,
            Some(carol),
            M::POST,
            "/v1/conversations",
            Some(
                json!({ "contextClass": "GROUP", "securityProfile": "MANAGED",
                "context": { "namespace": "privacy-relay", "reference": ctx } }),
            ),
        )
        .await;
    assert_eq!(r.reason(), "CONTEXT_AUTHORITY_UNKNOWN", "{}", r.text);
    let r = st
        .customer(
            &other_token,
            Some(carol),
            M::GET,
            &format!("/v1/conversations/{cid}"),
            None,
        )
        .await;
    assert!(r.status == 404 || r.status == 403, "{}", r.text);

    // 2. SUSPEND locks the conversation: authorization is lost for sending.
    lifecycle(ctx.clone(), "suspend", 1).await;
    eventually(20, "SUSPENDED applied", || async {
        (lifecycle_state(cid.clone()).await == "LOCKED").then_some(())
    })
    .await;
    let r = st.send_text(&token, bob, &cid, "while suspended").await;
    assert!(r.status >= 400 && r.status < 500, "{}", r.text);
    // 3. RESUME unlocks it.
    lifecycle(ctx.clone(), "resume", 2).await;
    eventually(20, "ACTIVE re-applied", || async {
        (lifecycle_state(cid.clone()).await == "ACTIVE").then_some(())
    })
    .await;
    let r = st.send_text(&token, bob, &cid, "resumed").await;
    assert_eq!(r.status, 201, "{}", r.text);
    // 4. REVOKE closes it; it cannot be reopened and the context cannot be reused.
    let started = std::time::Instant::now();
    lifecycle(ctx.clone(), "revoke", 3).await;
    eventually(20, "REVOKED applied", || async {
        (lifecycle_state(cid.clone()).await == "CLOSED").then_some(())
    })
    .await;
    println!(
        "EVIDENCE scenario4 revocation_propagation_ms={}",
        started.elapsed().as_millis()
    );
    let r = st.send_text(&token, alice, &cid, "after revoke").await;
    assert!(r.status >= 400 && r.status < 500, "{}", r.text);
    let v = scalar(
        st.mdb(),
        &format!("SELECT version::bigint FROM messaging_conversations WHERE id = '{cid}'"),
    )
    .await;
    let r = st
        .customer(
            &token,
            Some(alice),
            M::PATCH,
            &format!("/v1/conversations/{cid}"),
            Some(json!({ "expectedVersion": v, "lifecycleState": "ACTIVE" })),
        )
        .await;
    assert!(r.status >= 400 && r.status < 500, "{}", r.text);
    assert_eq!(lifecycle_state(cid.clone()).await, "CLOSED");
    assert_eq!(
        create_conv(ctx.clone()).await.reason(),
        "CONTEXT_AUTHORITY_WITHDRAWN"
    );
    // Every version was applied exactly once, in order, by the real worker (Privacy Relay
    // records APPLIED just after Messaging answered, so this may lag the close).
    eventually(20, "all four versions recorded as applied", || async {
        (scalar(&relay_db, &format!(
            "SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx}' AND status = 'APPLIED'"
        ))
        .await
            == 4)
            .then_some(())
    })
    .await;
    assert_eq!(
        scalar(
            &relay_db,
            &format!(
            "SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx}'"
        )
        )
        .await,
        4,
        "exactly one job per version"
    );
    assert_eq!(
        text(st.mdb(), &format!(
            "SELECT state || ':' || source_version::text FROM messaging_context_authorities WHERE environment_id = '{}' AND reference = '{ctx}'",
            t.env
        ))
        .await
        .as_deref(),
        Some("REVOKED:4")
    );

    // 5. Safe failure: Messaging is DOWN when a second context is revoked.
    let ctx2 = create_context().await;
    let conv2 = eventually(20, "second context ACTIVE", || async {
        let r = create_conv(ctx2.clone()).await;
        (r.status == 201).then_some(r)
    })
    .await;
    let cid2 = conv2.json["conversationId"].as_str().unwrap().to_string();
    st.stop_messaging().await;
    lifecycle(ctx2.clone(), "revoke", 1).await;
    // The revocation is committed in Privacy Relay; the push is retried, never dropped,
    // and never reported as applied.
    eventually(20, "push failure recorded with backoff", || async {
        (scalar(&relay_db, &format!(
            "SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx2}' \
             AND authority_state = 'REVOKED' AND status = 'PENDING' AND attempt_count >= 1 \
             AND last_error_category = 'UNAVAILABLE'"
        ))
        .await
            == 1)
            .then_some(())
    })
    .await;
    // Messaging comes back on the same address: the retry closes the conversation.
    st.start_messaging(&[]).await;
    eventually(
        60,
        "revocation applied after Messaging recovery",
        || async { (lifecycle_state(cid2.clone()).await == "CLOSED").then_some(()) },
    )
    .await;
    // Privacy Relay records APPLIED after Messaging answered (it may lag the close).
    eventually(20, "Privacy Relay records the job as applied", || async {
        (scalar(&relay_db, &format!(
            "SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx2}' AND status <> 'APPLIED'"
        ))
        .await
            == 0)
            .then_some(())
    })
    .await;
    // Nothing in Messaging names a contact value or Privacy Relay internals.
    let authorities = text(st.mdb(), &format!(
        "SELECT string_agg(row_to_json(a)::text, ',') FROM messaging_context_authorities a WHERE environment_id = '{}'",
        t.env
    ))
    .await
    .unwrap();
    // Ids and states only: no E.164 value, no contact field, no participant identity.
    assert!(!authorities.contains("\"+"), "{authorities}");
    assert!(
        !authorities.to_lowercase().contains("contact\""),
        "{authorities}"
    );
    assert!(!authorities.contains(&bob.to_string()), "{authorities}");
    assert!(!authorities.contains(&alice.to_string()), "{authorities}");
    drop(relay);
}

/// Scenario 4b — Privacy Relay outage and restart (real processes). Messaging keeps
/// serving what was authorized while Privacy Relay is down and never grants an unknown
/// context; a revocation committed while Messaging is down survives a Privacy Relay
/// restart (its persisted job) and is applied on recovery: sends are denied, realtime
/// subscriptions are revoked (§37) and cannot be re-opened, history stays readable; a
/// re-delivered or older authority version changes nothing.
#[tokio::test]
async fn scenario_4b_privacy_relay_outage_and_restart_neither_grant_nor_lose_authority() {
    let Some(b) = bins(&["PRIVACY_RELAY_BIN", "PRIVACY_RELAY_MIGRATION_BIN"]) else {
        return;
    };
    let Some(mut st) = Stack::new(&[]).await else {
        return;
    };
    use cf_service_auth::services as s;
    use reqwest::Method as M;
    let relay_key = key(&st.dir, "relay-key.pem", "privacy-relay-xs-1");
    let calling_key = key(&st.dir, "calling-key.pem", "calling-xs-1");
    let rjwk = relay_key.public_jwk();
    let cjwk = calling_key.public_jwk();
    st.messaging_trust(&[(
        s::PRIVACY_RELAY,
        &rjwk,
        &["messaging-context-authority.apply"],
    )]);
    st.start_messaging(&[]).await;
    std::fs::write(
        st.dir.join("relay-trust.json"),
        trust_bundle(
            s::PRIVACY_RELAY,
            &[(
                s::CALLING,
                &cjwk,
                &[
                    "communication.authorize",
                    "communication-context.read",
                    "communication-context.write",
                    "relay-session.read",
                    "relay-session.write",
                ],
            )],
        ),
    )
    .unwrap();
    let relay_db_url = create_db(&st.admin, "xs_relay_b").await;
    migrate(&b[1], &relay_db_url);
    let relay_db = Database::connect(&relay_db_url).await.unwrap();
    let relay_env: Vec<(String, String)> = [
        ("DATABASE_URL", relay_db_url.clone()),
        ("PRIVACY_RELAY_DATABASE_MAX_CONNECTIONS", "4".into()),
        (
            "PRIVACY_RELAY_METRICS_ADDRESS",
            format!("127.0.0.1:{}", free_port()),
        ),
        ("PRIVACY_RELAY_ENABLED", "true".into()),
        ("PRIVACY_RELAY_WORKER_INTERVAL_MS", "200".into()),
        ("CF_RUNTIME_ENVIRONMENT", env_name()),
        ("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-xs-1".into()),
        ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", st.path("relay-key.pem")),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", st.path("relay-trust.json")),
        ("MESSAGING_INTERNAL_BASE_URL", st.m_url()),
        ("RUST_LOG", "privacy_relay=warn".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let relay_http = st.http.clone();
    let relay_bin = b[0].clone();
    let start_relay = || {
        spawn(
            &relay_http,
            &relay_bin,
            "/health",
            "PRIVACY_RELAY_BIND_ADDRESS",
            &relay_env,
            None,
        )
    };
    let mut relay = start_relay().await;
    let calling = signer(ServiceId::calling(), calling_key);
    let as_relay = signer(ServiceId::privacy_relay(), relay_key);

    let t = st.tenant("EXTERNAL", &MESSAGING_SCOPES).await;
    let token = st.credential(&t, &MESSAGING_SCOPES).await;
    let alice = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let bob = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    st.allow_managed(&token, "GROUP").await;
    let http = st.http.clone();
    let relay_call = |url: String, path: String, body: Value| {
        signed(
            &http,
            &calling,
            ServiceId::privacy_relay(),
            Some(t.delegated()),
            M::POST,
            format!("{url}{path}"),
        )
        .json(&body)
    };
    let create_context = |url: String| {
        let r = relay_call(
            url,
            "/internal/v1/contexts".into(),
            json!({ "idempotencyKey": Uuid::new_v4(), "request": {
                "externalContextReference": format!("ref-{}", Uuid::new_v4().simple()),
                "participants": [{ "principalId": alice }, { "principalId": bob }],
                "allowedChannels": ["VOICE", "MESSAGING"], "directionPolicy": "BIDIRECTIONAL",
                "startsAt": now_plus(-1), "expiresAt": now_plus(60) } }),
        );
        async move {
            let r = send(r).await;
            assert_eq!(r.status, 201, "{}", r.text);
            r.json["context"]["contextId"].as_str().unwrap().to_string()
        }
    };
    let conv_body = |reference: &str| {
        json!({ "contextClass": "GROUP", "securityProfile": "MANAGED",
                "context": { "namespace": "privacy-relay", "reference": reference } })
    };
    let mdb = st.mdb().clone();
    let state_of = |cid: String| {
        let db = mdb.clone();
        async move {
            text(
                &db,
                &format!("SELECT lifecycle_state FROM messaging_conversations WHERE id = '{cid}'"),
            )
            .await
            .unwrap()
        }
    };

    // An ACTIVE context and its conversation.
    let ctx = create_context(relay.url.clone()).await;
    let conv = eventually(20, "ACTIVE authority applied", || async {
        let r = st
            .customer(
                &token,
                Some(alice),
                M::POST,
                "/v1/conversations",
                Some(conv_body(&ctx)),
            )
            .await;
        (r.status == 201).then_some(r)
    })
    .await;
    let cid = conv.json["conversationId"].as_str().unwrap().to_string();
    st.add_member(&token, alice, &cid, bob).await;

    // 1. Privacy Relay is DOWN: what was authorized keeps working; nothing new is granted.
    relay.stop().await;
    let r = st.send_text(&token, alice, &cid, "relay is down").await;
    assert_eq!(r.status, 201, "{}", r.text);
    let r = st
        .customer(
            &token,
            Some(alice),
            M::POST,
            "/v1/conversations",
            Some(conv_body(&Uuid::new_v4().to_string())),
        )
        .await;
    assert_eq!(r.reason(), "CONTEXT_AUTHORITY_UNKNOWN", "{}", r.text);

    // 2. A revocation committed while Messaging is down, then Privacy Relay restarts.
    relay = start_relay().await;
    st.stop_messaging().await;
    let revoke = send(relay_call(
        relay.url.clone(),
        format!("/internal/v1/contexts/{ctx}/revoke"),
        json!({ "idempotencyKey": Uuid::new_v4(), "request": { "expectedVersion": 1 } }),
    ))
    .await;
    assert_eq!(revoke.status, 200, "{}", revoke.text);
    eventually(20, "the push failed and is pending", || async {
        (scalar(&relay_db, &format!(
            "SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx}' \
             AND authority_state = 'REVOKED' AND status = 'PENDING' AND attempt_count >= 1"
        ))
        .await
            == 1)
            .then_some(())
    })
    .await;
    relay.stop().await;
    st.start_messaging(&[]).await;
    // Not applied yet (Privacy Relay is down): not lost either — the job is persisted.
    assert_eq!(state_of(cid.clone()).await, "ACTIVE");
    // Bob is connected and subscribed when the revocation finally arrives.
    let bob_device = st.device(&token, bob).await;
    let ct = st
        .customer(
            &token,
            Some(bob),
            M::POST,
            "/v1/messaging/client-tokens",
            Some(json!({ "deviceId": bob_device.device_id() })),
        )
        .await;
    assert_eq!(ct.status, 201, "{}", ct.text);
    let tk = send(
        st.http
            .post(format!("{}/v1/realtime/tickets", st.m_url()))
            .header(
                "authorization",
                format!("CF-Client-Token {}", ct.json["token"].as_str().unwrap()),
            ),
    )
    .await;
    assert_eq!(tk.status, 201, "{}", tk.text);
    let addr: std::net::SocketAddr = st.m_url().trim_start_matches("http://").parse().unwrap();
    let mut ws = common::realtime::open_ws(addr, tk.json["ticket"].as_str().unwrap(), None)
        .await
        .expect("authenticated WebSocket");
    let sub = ws.subscribe(&json!({ "conversationId": cid }), None).await;
    assert_eq!(sub["type"], "subscribed", "{sub}");
    let started = std::time::Instant::now();
    relay = start_relay().await;
    eventually(
        60,
        "the persisted revocation applied after the restart",
        || async { (state_of(cid.clone()).await == "CLOSED").then_some(()) },
    )
    .await;
    println!(
        "EVIDENCE scenario4b restart_to_revocation_ms={}",
        started.elapsed().as_millis()
    );
    let revoked = ws.next_of("subscription.revoked").await;
    assert_eq!(revoked["reason"], "CONTEXT_REVOKED", "{revoked}");
    ws.send(json!({ "type": "subscribe", "conversationId": cid, "requestId": "again" }))
        .await;
    let refused = ws.next_of("error").await;
    assert_eq!(refused["code"], "CONVERSATION_NOT_ACTIVE", "{refused}");
    let r = st
        .send_text(&token, bob, &cid, "after the revocation")
        .await;
    assert!(r.status >= 400 && r.status < 500, "{}", r.text);
    let history = st
        .customer(
            &token,
            Some(bob),
            M::GET,
            &format!("/v1/conversations/{cid}/messages"),
            None,
        )
        .await;
    assert_eq!(history.status, 200, "history is retained: {}", history.text);
    assert!(history.text.contains("relay is down"));
    eventually(20, "Privacy Relay records the job as applied", || async {
        (scalar(&relay_db, &format!(
            "SELECT count(*) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx}' AND status <> 'APPLIED'"
        ))
        .await
            == 0)
            .then_some(())
    })
    .await;

    // 3. Re-delivery of the applied version, and an older ACTIVE version: no change.
    let version = scalar(&relay_db, &format!(
        "SELECT max(context_version) FROM privacy_relay_messaging_authority_jobs WHERE context_id = '{ctx}'"
    ))
    .await;
    let conv_version = |db: DatabaseConnection, cid: String| async move {
        scalar(
            &db,
            &format!("SELECT version FROM messaging_conversations WHERE id = '{cid}'"),
        )
        .await
    };
    let before = conv_version(st.mdb().clone(), cid.clone()).await;
    let events = || {
        let db = st.mdb().clone();
        let cid = cid.clone();
        async move {
            scalar(&db, &format!(
                "SELECT count(*) FROM messaging_outbox_events WHERE conversation_id = '{cid}' AND event_type = 'messaging.conversation.lifecycle-changed'"
            ))
            .await
        }
    };
    let events_before = events().await;
    for (state, v) in [("REVOKED", version), ("ACTIVE", 1)] {
        let r = send(
            signed(&st.http, &as_relay, ServiceId::messaging(), Some(t.delegated()), M::POST,
                   format!("{}/internal/v1/context-authorities", st.m_url()))
            .json(&json!({ "namespace": "privacy-relay", "reference": ctx, "state": state, "sourceVersion": v })),
        )
        .await;
        assert_eq!(r.status, 200, "{state} v{v}: {}", r.text);
        assert_eq!(
            (r.json["applied"].as_bool(), r.json["reason"].as_str()),
            (Some(false), Some("STALE_VERSION")),
            "{}",
            r.text
        );
    }
    assert_eq!(state_of(cid.clone()).await, "CLOSED");
    assert_eq!(conv_version(st.mdb().clone(), cid.clone()).await, before);
    assert_eq!(
        events().await,
        events_before,
        "no duplicate lifecycle event"
    );
    drop(relay);
}

// =====================================================================================
// Scenario 5 — Managed SMS Bridge (real Messaging + real SMS Gateway, synthetic carrier)
// =====================================================================================

/// HMAC-SHA256 (RFC 2104) over sha2 — the synthetic carrier's callback signature.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

const PARTY: &str = "+15550123456";
const DCN: &str = "+15550100001";

/// Numbering's routing answers for ONE number assigned to ONE environment (Numbering is
/// not under test here: the SMS Gateway's real Numbering client talks to this stub).
#[derive(Clone)]
struct NumberingStub {
    number: Uuid,
    assignment: Uuid,
    scope: Value,
    app: Uuid,
}

async fn numbering_stub(stub: NumberingStub) -> String {
    use axum::{
        extract::{Path as AxPath, State},
        routing::{get, post},
        Json, Router,
    };
    async fn sender_grant(State(s): State<NumberingStub>, Json(b): Json<Value>) -> Json<Value> {
        assert_eq!(b["numberResourceId"], json!(s.number));
        Json(
            json!({ "grantKind": "SMS_SENDER", "grant": format!("sender-{}", Uuid::new_v4().simple()) }),
        )
    }
    async fn verify(State(s): State<NumberingStub>, Json(b): Json<Value>) -> Json<Value> {
        if b["consume"] == true {
            Json(
                json!({ "valid": true, "grantKind": "SMS_SENDER", "consumed": true, "e164": DCN,
                "numberResourceId": s.number, "assignmentId": s.assignment, "routingRevision": 1,
                "grantId": Uuid::new_v4(), "scope": s.scope }),
            )
        } else {
            Json(
                json!({ "valid": true, "grantKind": "INBOUND_SMS", "grantId": inbound_grant(&s),
                "numberResourceId": s.number, "assignmentId": s.assignment, "routingRevision": 1,
                "scope": s.scope, "target": { "kind": "APPLICATION", "ref": s.app } }),
            )
        }
    }
    fn inbound_grant(s: &NumberingStub) -> Uuid {
        Uuid::from_u128(s.number.as_u128() ^ 1)
    }
    async fn assignment(State(s): State<NumberingStub>, AxPath(id): AxPath<Uuid>) -> Json<Value> {
        assert_eq!(id, s.number);
        Json(
            json!({ "assignmentId": s.assignment, "assignmentStatus": "ACTIVE",
            "numberActive": true, "outbound": { "sms": true } }),
        )
    }
    async fn resolve(State(s): State<NumberingStub>, Json(b): Json<Value>) -> Json<Value> {
        if b["destination"] != DCN {
            return Json(json!({ "outcome": "NOT_ROUTED" }));
        }
        Json(
            json!({ "outcome": "ROUTED", "numberResourceId": s.number, "assignmentId": s.assignment,
            "routingRevision": 1, "scope": s.scope, "target": { "kind": "APPLICATION", "ref": s.app },
            "grant": { "grantId": inbound_grant(&s), "grant": "inbound-grant" } }),
        )
    }
    let app = Router::new()
        .route("/internal/v1/routing/sms-sender-grants", post(sender_grant))
        .route("/internal/v1/routing/grant-verifications", post(verify))
        .route(
            "/internal/v1/routing/number-assignments/:id",
            get(assignment),
        )
        .route(
            "/internal/v1/routing/sms-destination-resolutions",
            post(resolve),
        )
        .with_state(stub);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn scenario_5_managed_sms_bridge_between_real_messaging_and_real_sms_gateway() {
    let Some(b) = bins(&["SMS_GATEWAY_BIN", "SMS_GATEWAY_MIGRATION_BIN"]) else {
        return;
    };
    let Some(mut st) = Stack::new(&[]).await else {
        return;
    };
    use cf_service_auth::services as s;
    use reqwest::Method as M;
    let b64 =
        |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let gw_key = key(&st.dir, "gw-key.pem", "sms-gateway-xs-1");
    let callback_secret: [u8; 32] = rand::random();
    std::fs::write(st.dir.join("gw-callback.secret"), b64(&callback_secret)).unwrap();
    std::fs::write(
        st.dir.join("gw-content.key"),
        b64(&rand::random::<[u8; 32]>()),
    )
    .unwrap();
    std::fs::write(
        st.dir.join("gw-fingerprint.key"),
        b64(&rand::random::<[u8; 32]>()),
    )
    .unwrap();
    let gjwk = gw_key.public_jwk();
    let mjwk = st.messaging_key.public_jwk();
    // Messaging grants the gateway EXACTLY inbound delivery; the gateway grants Messaging
    // EXACTLY bridge submission.
    st.messaging_trust(&[(s::SMS_GATEWAY, &gjwk, &["messaging-sms-bridge.deliver"])]);
    std::fs::write(
        st.dir.join("gw-trust.json"),
        trust_bundle(
            s::SMS_GATEWAY,
            &[(s::MESSAGING, &mjwk, &["sms-bridge.submit"])],
        ),
    )
    .unwrap();

    let t = st.tenant("EXTERNAL", &MESSAGING_SCOPES).await;
    let token = st.credential(&t, &MESSAGING_SCOPES).await;
    let owner = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let member = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let party = {
        let p = st
            .created(
                &format!("/internal/v1/admin/environments/{}/principals", t.env),
                json!({ "kind": "TELEPHONE_ENDPOINT", "externalReference": Uuid::new_v4().to_string() }),
            )
            .await;
        Uuid::parse_str(p["principal"]["id"].as_str().unwrap()).unwrap()
    };
    let number = Uuid::new_v4();
    let numbering = numbering_stub(NumberingStub {
        number,
        assignment: Uuid::new_v4(),
        scope: json!({ "tenantId": t.tenant, "applicationId": t.app, "environmentId": t.env }),
        app: t.app,
    })
    .await;

    // Messaging needs the gateway's address and vice versa: fix the gateway's port first.
    let gw_port = free_port();
    let gw_url = format!("http://127.0.0.1:{gw_port}");
    st.start_messaging(&[("CF_SMS_GATEWAY_BASE_URL", gw_url.clone())])
        .await;
    let gw_db_url = create_db(&st.admin, "xs_sms").await;
    migrate(&b[1], &gw_db_url);
    let gw_db = Database::connect(&gw_db_url).await.unwrap();
    let gw_env: Vec<(String, String)> = [
        ("DATABASE_URL", gw_db_url.clone()),
        ("SMS_GATEWAY_DATABASE_MAX_CONNECTIONS", "5".into()),
        (
            "SMS_GATEWAY_METRICS_ADDRESS",
            format!("127.0.0.1:{}", free_port()),
        ),
        ("CF_RUNTIME_ENVIRONMENT", env_name()),
        ("CF_SERVICE_AUTH_KEY_ID", "sms-gateway-xs-1".into()),
        ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", st.path("gw-key.pem")),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", st.path("gw-trust.json")),
        ("CF_PLATFORM_ACCESS_BASE_URL", st.pa.url.clone()),
        ("CF_NUMBERING_BASE_URL", numbering.clone()),
        ("CF_MESSAGING_BASE_URL", st.m_url()),
        ("SMS_GATEWAY_CONTENT_KEY_ID", "xs-content-1".into()),
        ("SMS_GATEWAY_CONTENT_KEY_FILE", st.path("gw-content.key")),
        (
            "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
            st.path("gw-fingerprint.key"),
        ),
        ("SMS_GATEWAY_PROVIDER", "deterministic".into()),
        (
            "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
            st.path("gw-callback.secret"),
        ),
        // Outbound #1 accepted; #2 times out AFTER the carrier created it (unknown outcome).
        (
            "SMS_GATEWAY_DETERMINISTIC_SCRIPT",
            "ACCEPT,TIMEOUT_AFTER_CREATE".into(),
        ),
        ("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES", "+1".into()),
        ("SMS_GATEWAY_WORKER_INTERVAL_MS", "100".into()),
        ("SMS_GATEWAY_RETRY_BASE_MS", "100".into()),
        ("SMS_GATEWAY_SUBMIT_TIMEOUT_MS", "300".into()),
        // Spend: two single-segment messages fit, a two-segment third does not.
        ("SMS_GATEWAY_DAILY_SPEND_LIMIT_MINOR", "12".into()),
        ("RUST_LOG", "sms_gateway=warn".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let _gw = spawn(
        &st.http,
        &b[0],
        "/ready",
        "SMS_GATEWAY_BIND_ADDRESS",
        &gw_env,
        Some(gw_port),
    )
    .await;

    // Everything Messaging returns to the customer, checked for leaks at the end.
    let mut public = String::new();
    let carrier = |kind: &'static str, event_id: String, body: Value| {
        let raw = serde_json::to_vec(&body).unwrap();
        let ts = chrono::Utc::now().timestamp().to_string();
        let mut msg = format!("v1:{ts}:{event_id}:").into_bytes();
        msg.extend_from_slice(&raw);
        let sig = format!("v1={}", hex::encode(hmac_sha256(&callback_secret, &msg)));
        let r = st
            .http
            .post(format!(
                "{gw_url}/provider-callbacks/v1/sms/deterministic/{kind}"
            ))
            .header("content-type", "application/json")
            .header("x-provider-event-id", event_id)
            .header("x-provider-timestamp", ts)
            .header("x-provider-signature", sig)
            .body(raw);
        send(r)
    };
    let inbound = |text: &str| {
        let id = format!("in-{}", Uuid::new_v4().simple());
        let body = json!({ "kind": "INBOUND", "eventId": id, "from": PARTY, "to": DCN, "text": text,
            "receivedAt": now_plus(0) });
        (id.clone(), carrier("inbound", id, body))
    };
    let receipt = |provider_ref: String, status: &str| {
        let id = format!("rc-{}", Uuid::new_v4().simple());
        let body = json!({ "kind": "RECEIPT", "eventId": id, "messageRef": provider_ref,
            "status": status, "errorCode": null, "costMinor": null, "occurredAt": now_plus(0) });
        (id.clone(), body)
    };

    st.allow_managed(&token, "GROUP").await;
    let conv = st
        .customer(
            &token,
            Some(owner),
            M::POST,
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
        )
        .await;
    assert_eq!(conv.status, 201, "{}", conv.text);
    let cid = conv.json["conversationId"].as_str().unwrap().to_string();
    st.add_member(&token, owner, &cid, member).await;
    let bridge_body = json!({ "numberResourceId": number, "to": PARTY, "externalPrincipalId": party,
        "disclosureVersion": messaging::sms_bridge::DISCLOSURE_VERSION, "smsDisclosureAcknowledged": true });
    // Never silently: without the explicit disclosure acknowledgement nothing is bridged.
    let mut unack = bridge_body.clone();
    unack["smsDisclosureAcknowledged"] = json!(false);
    let r = st
        .customer(
            &token,
            Some(owner),
            M::POST,
            &format!("/v1/conversations/{cid}/sms-bridges"),
            Some(unack),
        )
        .await;
    assert_eq!(r.reason(), "SMS_DISCLOSURE_REQUIRED", "{}", r.text);
    let r = st
        .customer(
            &token,
            Some(owner),
            M::POST,
            &format!("/v1/conversations/{cid}/sms-bridges"),
            Some(bridge_body.clone()),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text);
    assert_eq!(r.json["encryption"]["endToEnd"], false);
    assert_eq!(r.json["securityProfile"], "MANAGED");
    public.push_str(&r.text);
    let bridge_id = r.json["bridgeId"].as_str().unwrap().to_string();
    // The gateway holds the binding (number sealed, never in plaintext).
    assert_eq!(
        scalar(
            &gw_db,
            &format!("SELECT count(*) FROM sms_bridge_bindings WHERE bridge_ref = '{bridge_id}'")
        )
        .await,
        1
    );

    // E2EE is never bridged (the creator holds a real client device).
    let _owner_device = st.device(&token, owner).await;
    let e2ee = st
        .customer(
            &token,
            Some(owner),
            M::POST,
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP", "securityProfile": "PRIVATE_E2EE" })),
        )
        .await;
    if e2ee.status == 201 {
        let ecid = e2ee.json["conversationId"].as_str().unwrap();
        let r = st
            .customer(
                &token,
                Some(owner),
                M::POST,
                &format!("/v1/conversations/{ecid}/sms-bridges"),
                Some(bridge_body.clone()),
            )
            .await;
        assert_eq!(r.reason(), "E2EE_CONVERSATION_NOT_BRIDGEABLE", "{}", r.text);
    } else {
        panic!(
            "could not create an E2EE conversation to prove refusal: {}",
            e2ee.text
        );
    }

    let transport = |mid: String| {
        let st = &st;
        let token = token.clone();
        let cid = cid.clone();
        async move {
            st.customer(
                &token,
                Some(owner),
                M::GET,
                &format!("/v1/conversations/{cid}/messages/{mid}/sms-transport"),
                None,
            )
            .await
        }
    };
    let transport_id = |mid: String| {
        let db = st.mdb().clone();
        async move {
            text(&db, &format!(
                "SELECT sms_message_id::text FROM messaging_sms_transports WHERE message_id = '{mid}' AND direction = 'OUTBOUND'"
            ))
            .await
        }
    };

    // 1. Outbound: in-app message → SMS → carrier DELIVERED receipt → Messaging.
    let started = std::time::Instant::now();
    let m1 = st.send_text(&token, member, &cid, "hello over sms").await;
    assert_eq!(m1.status, 201, "{}", m1.text);
    let mid1 = m1.json["messageId"].as_str().unwrap().to_string();
    let sms1 = eventually(30, "message 1 submitted to the gateway", || async {
        transport_id(mid1.clone()).await
    })
    .await;
    let provider_ref = eventually(30, "carrier accepted message 1", || async {
        text(
            &gw_db,
            &format!(
            "SELECT provider_message_ref FROM sms_provider_bindings WHERE transport_id = '{sms1}'"
        ),
        )
        .await
    })
    .await;
    let (rid, rbody) = receipt(provider_ref.clone(), "DELIVERED");
    let r = carrier("delivery-receipts", rid.clone(), rbody.clone()).await;
    assert_eq!(
        (r.status, r.json["status"].as_str()),
        (200, Some("ACCEPTED")),
        "{}",
        r.text
    );
    // A duplicate carrier event changes nothing.
    let r = carrier("delivery-receipts", rid, rbody).await;
    assert_eq!(r.json["status"], "DUPLICATE", "{}", r.text);
    let t1 = eventually(60, "DELIVERED reaches Messaging", || async {
        let r = transport(mid1.clone()).await;
        (r.json["transportStatus"] == "DELIVERED").then_some(r)
    })
    .await;
    println!(
        "EVIDENCE scenario5 outbound_delivered_visible_ms={}",
        started.elapsed().as_millis()
    );
    assert_eq!(t1.json["inAppStateIndependent"], true);
    public.push_str(&t1.text);

    // 2. Inbound: carrier → gateway → Messaging as a MANAGED SMS message (exactly once).
    let (in_id, r) = inbound("reply from the phone");
    let r = r.await;
    assert_eq!(r.status, 200, "{}", r.text);
    let in_body = json!({ "kind": "INBOUND", "eventId": in_id, "from": PARTY, "to": DCN,
        "text": "reply from the phone", "receivedAt": now_plus(0) });
    let dup = carrier("inbound", in_id, in_body).await;
    assert_eq!(dup.json["status"], "DUPLICATE", "{}", dup.text);
    let listing = eventually(30, "inbound SMS in the conversation", || async {
        let r = st
            .customer(
                &token,
                Some(owner),
                M::GET,
                &format!("/v1/conversations/{cid}/messages"),
                None,
            )
            .await;
        r.text.contains("reply from the phone").then_some(r)
    })
    .await;
    assert!(listing.text.contains("\"SMS\""), "{}", listing.text);
    assert!(
        listing.text.contains(&party.to_string()),
        "{}",
        listing.text
    );
    public.push_str(&listing.text);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        scalar(st.mdb(), &format!(
            "SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND channel = 'SMS'"
        ))
        .await,
        1,
        "duplicate carrier event delivered once"
    );

    // 3. Unknown outcome: the carrier created message 2 but the submit timed out.
    let m2 = st.send_text(&token, member, &cid, "second").await;
    assert_eq!(m2.status, 201, "{}", m2.text);
    let mid2 = m2.json["messageId"].as_str().unwrap().to_string();
    let sms2 = eventually(30, "message 2 submitted", || async {
        transport_id(mid2.clone()).await
    })
    .await;
    eventually(30, "gateway records UNKNOWN", || async {
        (text(
            &gw_db,
            &format!("SELECT status FROM sms_transports WHERE id = '{sms2}'"),
        )
        .await
        .as_deref()
            == Some("UNKNOWN"))
        .then_some(())
    })
    .await;
    // Never falsely terminal in Messaging while unknown.
    let r = transport(mid2.clone()).await;
    assert!(
        !["DELIVERED", "FAILED"].contains(&r.json["transportStatus"].as_str().unwrap_or("")),
        "{}",
        r.text
    );
    // Read-back reconciliation (after the gateway's fixed delay) recovers SUBMITTED.
    let started = std::time::Instant::now();
    eventually(90, "reconciled after unknown outcome", || async {
        (text(
            &gw_db,
            &format!("SELECT status FROM sms_transports WHERE id = '{sms2}'"),
        )
        .await
        .as_deref()
            == Some("SUBMITTED"))
        .then_some(())
    })
    .await;
    println!(
        "EVIDENCE scenario5 unknown_outcome_reconciled_ms={}",
        started.elapsed().as_millis()
    );

    // 4. Opt-out: the party texts STOP; the next bridged message is BLOCKED, in-app is not.
    let (_, r) = inbound("STOP");
    assert_eq!(r.await.status, 200);
    let m3 = st.send_text(&token, member, &cid, "after stop").await;
    assert_eq!(
        m3.status, 201,
        "in-app delivery is independent of SMS: {}",
        m3.text
    );
    let mid3 = m3.json["messageId"].as_str().unwrap().to_string();
    let t3 = eventually(60, "opt-out refusal reaches Messaging", || async {
        let r = transport(mid3.clone()).await;
        (r.json["transportStatus"] == "BLOCKED").then_some(r)
    })
    .await;
    assert_eq!(
        t3.json["blockedReason"], "RECIPIENT_OPTED_OUT",
        "{}",
        t3.text
    );
    // 5. Spend: after START, a two-segment message would exceed the daily budget.
    let (_, r) = inbound("START");
    assert_eq!(r.await.status, 200);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let long = "x".repeat(200);
    let m4 = st.send_text(&token, member, &cid, &long).await;
    assert_eq!(m4.status, 201, "{}", m4.text);
    let mid4 = m4.json["messageId"].as_str().unwrap().to_string();
    let t4 = eventually(60, "spend refusal reaches Messaging", || async {
        let r = transport(mid4.clone()).await;
        (r.json["transportStatus"] == "BLOCKED").then_some(r)
    })
    .await;
    assert_eq!(
        t4.json["blockedReason"], "SPEND_LIMIT_EXCEEDED",
        "{}",
        t4.text
    );
    public.push_str(&t3.text);
    public.push_str(&t4.text);

    // 6. Close: nothing more is bridged.
    let r = st
        .customer(
            &token,
            Some(owner),
            M::POST,
            &format!("/v1/conversations/{cid}/sms-bridges/{bridge_id}/close"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    public.push_str(&r.text);
    let bridges = st
        .customer(
            &token,
            Some(owner),
            M::GET,
            &format!("/v1/conversations/{cid}/sms-bridges"),
            None,
        )
        .await;
    public.push_str(&bridges.text);
    let outbound_before = scalar(
        st.mdb(),
        "SELECT count(*) FROM messaging_sms_transports WHERE direction = 'OUTBOUND'",
    )
    .await;
    st.send_text(&token, member, &cid, "after close").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        scalar(
            st.mdb(),
            "SELECT count(*) FROM messaging_sms_transports WHERE direction = 'OUTBOUND'"
        )
        .await,
        outbound_before
    );

    // Privacy: no phone number or carrier identifier in Messaging's responses or database.
    for leak in ["0123456", "0100001", "dsms-", &provider_ref] {
        assert!(!public.contains(leak), "{leak} leaked: {public}");
    }
    let mdump = text(st.mdb(),
        "SELECT coalesce(string_agg(row_to_json(b)::text, ','), '') FROM messaging_sms_bridges b").await.unwrap_or_default()
        + &text(st.mdb(),
        "SELECT coalesce(string_agg(row_to_json(x)::text, ','), '') FROM messaging_sms_transports x").await.unwrap_or_default();
    for leak in ["0123456", "0100001", "dsms-"] {
        assert!(!mdump.contains(leak), "{leak} stored in Messaging: {mdump}");
    }
    let sealed = text(
        &gw_db,
        "SELECT coalesce(string_agg(row_to_json(b)::text, ','), '') FROM sms_bridge_bindings b",
    )
    .await
    .unwrap_or_default();
    assert!(!sealed.contains("0123456"), "{sealed}");
}

// =====================================================================================
// Scenario 9 — Webhook privacy (real Messaging → real Calling → signed customer webhook)
// =====================================================================================

/// One webhook request as the customer endpoint received it.
type Delivery = (axum::http::HeaderMap, Vec<u8>);

#[derive(Clone, Default)]
struct HookReceiver {
    seen: Arc<std::sync::Mutex<Vec<Delivery>>>,
    /// Statuses to answer, in order (then 200).
    script: Arc<std::sync::Mutex<Vec<u16>>>,
}

async fn hook_receiver() -> (HookReceiver, String) {
    use axum::{extract::State, http::StatusCode, routing::post, Router};
    async fn hook(
        State(r): State<HookReceiver>,
        headers: axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> StatusCode {
        r.seen.lock().unwrap().push((headers, body.to_vec()));
        let mut script = r.script.lock().unwrap();
        let status = if script.is_empty() {
            200
        } else {
            script.remove(0)
        };
        StatusCode::from_u16(status).unwrap()
    }
    let r = HookReceiver::default();
    let app = Router::new()
        .route("/hook", post(hook))
        .with_state(r.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (r, url)
}

#[tokio::test]
async fn scenario_9_messaging_events_reach_customer_webhooks_through_real_calling_without_content()
{
    RUNTIME.with(|r| r.set(RuntimeEnvironment::Local));
    let Some(b) = bins(&["CALLING_BIN", "CALLING_MIGRATION_BIN"]) else {
        return;
    };
    use cf_service_auth::services as s;
    use reqwest::Method as M;
    let dir_key = ServiceSigningKey::generate(KeyId::parse("calling-xs-1").unwrap());
    let cjwk = dir_key.public_jwk();
    let Some(mut st) = Stack::new(&[(
        s::CALLING,
        &cjwk,
        &[
            "platform-credential.authenticate",
            "platform-principal.resolve",
            "platform-principal.register",
            "platform-identity.read",
        ],
    )])
    .await
    else {
        return;
    };
    std::fs::write(
        st.dir.join("calling-key.pem"),
        dir_key.to_pkcs8_pem().as_bytes(),
    )
    .unwrap();
    let mjwk = st.messaging_key.public_jwk();
    st.messaging_trust(&[]);
    // Calling grants Messaging EXACTLY platform-event.publish.
    std::fs::write(
        st.dir.join("calling-trust.json"),
        trust_bundle(
            s::CALLING,
            &[(s::MESSAGING, &mjwk, &["platform-event.publish"])],
        ),
    )
    .unwrap();
    let wh_key: Vec<u8> = (0..32u8)
        .map(|i| 13u8.wrapping_add(i.wrapping_mul(7)))
        .collect();
    std::fs::write(
        st.dir.join("webhook.key"),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &wh_key),
    )
    .unwrap();
    std::fs::write(
        st.dir.join("signaling.key"),
        "xs-e2e-signaling-signing-key-5d2c8e1f0a9b7364",
    )
    .unwrap();
    let calling_db_url = create_db(&st.admin, "xs_calling").await;
    migrate(&b[1], &calling_db_url);
    let calling_db = Database::connect(&calling_db_url).await.unwrap();
    let calling_port = free_port();
    let calling_url = format!("http://127.0.0.1:{calling_port}");
    st.start_messaging(&[("CF_CALLING_BASE_URL", calling_url.clone())])
        .await;
    let calling_env: Vec<(String, String)> = [
        ("DATABASE_URL", calling_db_url.clone()),
        ("CALLING_DATABASE_MAX_CONNECTIONS", "5".into()),
        ("CF_RUNTIME_ENVIRONMENT", env_name()),
        ("CF_SERVICE_AUTH_KEY_ID", "calling-xs-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            st.path("calling-key.pem"),
        ),
        (
            "CF_SERVICE_TRUST_BUNDLE_FILE",
            st.path("calling-trust.json"),
        ),
        ("CF_PLATFORM_ACCESS_BASE_URL", st.pa.url.clone()),
        ("CALLING_WEBHOOK_SECRET_KEY_ID", "xs-webhooks-1".into()),
        ("CALLING_WEBHOOK_SECRET_KEY_FILE", st.path("webhook.key")),
        // Local runtime only: lets the delivery worker reach this test's loopback receiver.
        ("CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS", "true".into()),
        ("CALLING_IDENTITY_SYNC_INTERVAL_MS", "200".into()),
        (
            "CALLING_SIGNALING_SIGNING_KEY_FILE",
            st.path("signaling.key"),
        ),
        ("RUST_LOG", "calling=warn".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let _calling = spawn(
        &st.http,
        &b[0],
        "/health",
        "CALLING_BIND_ADDRESS",
        &calling_env,
        Some(calling_port),
    )
    .await;

    let mut scopes = MESSAGING_SCOPES.to_vec();
    scopes.extend(["webhooks.manage", "webhooks.read"]);
    let t = st.tenant("EXTERNAL", &scopes).await;
    let token = st.credential(&t, &scopes).await;
    let alice = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let bob = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    // A second tenant with its own subscription must never see tenant A's events.
    let other = st.tenant("EXTERNAL", &scopes).await;
    let other_token = st.credential(&other, &scopes).await;

    let (receiver, hook_url) = hook_receiver().await;
    let (other_receiver, other_url) = hook_receiver().await;
    let types = json!([
        "conversation.created",
        "conversation.member_added",
        "message.created"
    ]);
    let subscribe = |tok: String, url: String| {
        let r = st
            .http
            .post(format!("{calling_url}/v1/webhooks"))
            .bearer_auth(tok)
            .header("idempotency-key", Uuid::new_v4().to_string())
            .json(&json!({ "url": url, "eventTypes": types }));
        send(r)
    };
    let sub = subscribe(token.clone(), hook_url).await;
    assert_eq!(sub.status, 201, "{}", sub.text);
    let secret = sub.json["signingSecret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("whsec_"));
    let sub2 = subscribe(other_token.clone(), other_url).await;
    assert_eq!(sub2.status, 201, "{}", sub2.text);
    // A credential without webhooks.manage cannot subscribe (scope escalation denied).
    let weak = st.credential(&t, &["messages.read"]).await;
    let r = subscribe(weak, "http://127.0.0.1:9/hook".into()).await;
    assert_eq!(r.status, 403, "{}", r.text);

    // The first message.created attempt is answered 503 (retry path).
    receiver.script.lock().unwrap().extend([503]);
    st.allow_managed(&token, "GROUP").await;
    let started = std::time::Instant::now();
    let conv = st
        .customer(
            &token,
            Some(alice),
            M::POST,
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
        )
        .await;
    assert_eq!(conv.status, 201, "{}", conv.text);
    let cid = conv.json["conversationId"].as_str().unwrap().to_string();
    st.add_member(&token, alice, &cid, bob).await;
    // Wait for the first deliveries so the 503 lands on one of them.
    eventually(60, "first webhook delivery", || async {
        (!receiver.seen.lock().unwrap().is_empty()).then_some(())
    })
    .await;
    println!(
        "EVIDENCE scenario9 first_webhook_visible_ms={}",
        started.elapsed().as_millis()
    );
    const PLAINTEXT: &str = "north gate secret words";
    let m = st.send_text(&token, bob, &cid, PLAINTEXT).await;
    assert_eq!(m.status, 201, "{}", m.text);
    let mid = m.json["messageId"].as_str().unwrap().to_string();

    // The 503 schedules a retry (backoff ≥ 15 s); bring it forward in Calling's database
    // (test acceleration only; the retry itself is Calling's own worker).
    eventually(60, "a delivery waits for retry", || async {
        (scalar(
            &calling_db,
            "SELECT count(*) FROM webhook_deliveries WHERE status = 'RETRY_SCHEDULED'",
        )
        .await
            == 1)
            .then_some(())
    })
    .await;
    calling_db
        .execute_unprepared("UPDATE webhook_deliveries SET next_attempt_at = now() - interval '1 second' WHERE status = 'RETRY_SCHEDULED'")
        .await
        .unwrap();
    let seen = eventually(60, "all three events delivered", || async {
        let seen = receiver.seen.lock().unwrap().clone();
        let delivered = scalar(&calling_db, &format!(
            "SELECT count(*) FROM webhook_deliveries d JOIN webhook_subscriptions s ON s.id = d.subscription_id \
             WHERE d.status = 'SUCCEEDED' AND s.environment_id = '{}'", t.env)).await;
        (delivered == 3).then_some(seen)
    })
    .await;
    assert_eq!(seen.len(), 4, "3 events + 1 retried attempt");
    assert_eq!(
        scalar(&calling_db, "SELECT count(*) FROM webhook_delivery_attempts WHERE outcome = 'RETRY' AND http_status = 503").await,
        1
    );

    let header =
        |h: &axum::http::HeaderMap, k: &str| h.get(k).unwrap().to_str().unwrap().to_string();
    let mut types_seen = Vec::new();
    let mut delivery_ids = std::collections::HashMap::<String, Vec<String>>::new();
    for (h, body) in &seen {
        // Signed: v1=HMAC-SHA256(secret, "v1:{ts}:{deliveryId}:" + body).
        let ts = header(h, "cf-webhook-timestamp");
        let did = header(h, "cf-webhook-id");
        let mut msg = format!("v1:{ts}:{did}:").into_bytes();
        msg.extend_from_slice(body);
        assert_eq!(
            header(h, "cf-webhook-signature"),
            format!("v1={}", hex::encode(hmac_sha256(secret.as_bytes(), &msg))),
            "signature verifies"
        );
        // A forged body does not verify under the same signature.
        let mut forged = format!("v1:{ts}:{did}:").into_bytes();
        forged.extend_from_slice(&[body.as_slice(), b" "].concat());
        assert_ne!(
            header(h, "cf-webhook-signature"),
            format!(
                "v1={}",
                hex::encode(hmac_sha256(secret.as_bytes(), &forged))
            )
        );
        delivery_ids
            .entry(header(h, "cf-webhook-event-id"))
            .or_default()
            .push(did);
        let text = String::from_utf8(body.clone()).unwrap();
        // No content, ciphertext, key, device, epoch, credential or other tenant's ids.
        for leak in [
            "north gate",
            "secret words",
            "ciphertext",
            "\"text\"",
            "epoch",
            "device",
            "keyPackage",
            "whsec_",
            token.as_str(),
            &other.env.to_string(),
            &other.tenant.to_string(),
        ] {
            assert!(!text.contains(leak), "{leak} leaked: {text}");
        }
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["data"]["conversationId"], json!(cid));
        types_seen.push(v["type"].as_str().unwrap().to_string());
        if v["type"] == "message.created" {
            assert_eq!(v["data"]["messageId"], json!(mid));
        }
    }
    types_seen.sort();
    types_seen.dedup();
    assert_eq!(
        types_seen,
        [
            "conversation.created",
            "conversation.member_added",
            "message.created"
        ]
    );
    // The retried event kept its delivery id and event id (customer-side deduplication).
    let retried: Vec<_> = delivery_ids.values().filter(|d| d.len() == 2).collect();
    assert_eq!(retried.len(), 1);
    assert_eq!(retried[0][0], retried[0][1]);
    // Messaging relayed everything it queued; nothing waits or died.
    eventually(30, "relay drained", || async {
        (scalar(
            st.mdb(),
            "SELECT count(*) FROM messaging_webhook_relay WHERE status <> 'RELAYED'",
        )
        .await
            == 0)
            .then_some(())
    })
    .await;
    // The stored public events are ids-only too.
    let stored = text(&calling_db, "SELECT string_agg(body::text, ',') FROM webhook_events WHERE producer = 'cfmeet.messaging'").await.unwrap();
    assert!(!stored.contains("north gate"), "{stored}");
    // Tenant B received nothing of tenant A.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(other_receiver.seen.lock().unwrap().is_empty());
}

// =====================================================================================
// Failure injection — PostgreSQL loss during active messaging (real Messaging process)
// =====================================================================================

#[tokio::test]
async fn postgresql_loss_during_active_messaging_loses_nothing_acknowledged_and_duplicates_nothing()
{
    let Some(mut st) = Stack::new(&[]).await else {
        return;
    };
    use reqwest::Method as M;
    st.messaging_trust(&[]);
    // Messaging's OWN database, reached through the gate.
    let db = TestDb::empty().await.unwrap();
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    // postgres://user:pass@host:port/name — reroute host:port through the gate.
    let (head, rest) = db.url.split_once('@').unwrap();
    let (hostport, tail) = rest.split_once('/').unwrap();
    let hostport = hostport.replace("localhost", "127.0.0.1");
    let hostport = if hostport.contains(':') {
        hostport
    } else {
        format!("{hostport}:5432")
    };
    let target: std::net::SocketAddr = hostport.parse().unwrap();
    let gate = TcpGate::start(target).await;
    let gated_url = format!("{head}@127.0.0.1:{}/{tail}", gate.port);
    let direct = db.conn.clone();
    st.messaging_db = Some(db);
    st.start_messaging(&[
        ("DATABASE_URL", gated_url),
        ("MESSAGING_DATABASE_MAX_CONNECTIONS", "8".to_string()),
    ])
    .await;

    let t = st.tenant("EXTERNAL", &MESSAGING_SCOPES).await;
    let token = st.credential(&t, &MESSAGING_SCOPES).await;
    let alice = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let bob = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    st.allow_managed(&token, "GROUP").await;
    let conv = st
        .customer(
            &token,
            Some(alice),
            M::POST,
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
        )
        .await;
    assert_eq!(conv.status, 201, "{}", conv.text);
    let cid = conv.json["conversationId"].as_str().unwrap().to_string();
    st.add_member(&token, alice, &cid, bob).await;

    // 120 messages from 2 concurrent senders; the database is cut after ~40 of them.
    const N: usize = 120;
    let ids: Vec<(Uuid, Uuid)> = (0..N)
        .map(|i| (Uuid::new_v4(), if i % 2 == 0 { alice } else { bob }))
        .collect();
    let send = |client_id: Uuid, from: Uuid| {
        let http = st.http.clone();
        let url = format!("{}/v1/conversations/{cid}/messages", st.m_url());
        let token = token.clone();
        async move {
            let r = http
                .post(url)
                .bearer_auth(token)
                .header("cf-acting-principal", from.to_string())
                .header("idempotency-key", Uuid::new_v4().to_string())
                .timeout(Duration::from_secs(10))
                .json(&json!({ "clientMessageId": client_id, "text": format!("m-{client_id}") }))
                .send()
                .await;
            match r {
                Ok(r) => r.status().as_u16(),
                Err(_) => 0, // indeterminate (timeout / connection error)
            }
        }
    };
    let mut outcomes = vec![0u16; N];
    let started = std::time::Instant::now();
    let mut downtime = None;
    for chunk in (0..N).collect::<Vec<_>>().chunks(10) {
        if chunk[0] == 40 {
            // Cut while this batch is in flight (some transactions are mid-commit).
            let g = gate.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(15)).await;
                g.cut();
            });
            downtime = Some(std::time::Instant::now());
        }
        if chunk[0] == 80 {
            gate.restore();
        }
        let futs: Vec<_> = chunk.iter().map(|&i| send(ids[i].0, ids[i].1)).collect();
        for (i, s) in chunk.iter().zip(futures_util::future::join_all(futs).await) {
            outcomes[*i] = s;
        }
        if chunk[0] == 40 {
            // Readiness reports the database outage instead of lying.
            let r = st
                .http
                .get(format!("{}/ready", st.m_url()))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 503, "ready while the database is cut");
        }
    }
    let down_ms = downtime.map(|d| d.elapsed().as_millis()).unwrap_or(0);
    let acknowledged: Vec<usize> = (0..N).filter(|&i| outcomes[i] == 201).collect();
    let refused: Vec<usize> = (0..N).filter(|&i| outcomes[i] >= 500).collect();
    let indeterminate: Vec<usize> = (0..N).filter(|&i| outcomes[i] == 0).collect();
    let other: Vec<(usize, u16)> = (0..N)
        .filter(|&i| ![0, 201].contains(&outcomes[i]) && outcomes[i] < 500)
        .map(|i| (i, outcomes[i]))
        .collect();
    assert!(
        other.is_empty(),
        "unexpected outcomes during the outage: {other:?}"
    );
    assert!(
        !refused.is_empty() || !indeterminate.is_empty(),
        "the outage was observed"
    );
    // Readiness recovers.
    eventually(30, "ready after restore", || async {
        let r = st
            .http
            .get(format!("{}/ready", st.m_url()))
            .send()
            .await
            .ok()?;
        (r.status().as_u16() == 200).then_some(())
    })
    .await;
    // Every acknowledged message is durable; nothing un-acknowledged pretended success.
    let stored = |cm: Uuid| {
        let db = direct.clone();
        let cid = cid.clone();
        async move {
            scalar(&db, &format!(
                "SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND client_message_id = '{cm}'"
            ))
            .await
        }
    };
    for &i in &acknowledged {
        assert_eq!(
            stored(ids[i].0).await,
            1,
            "acknowledged message {i} is durable"
        );
    }
    // The client retries everything it was not sure about with the SAME client id.
    let mut retried_existing = 0;
    for i in refused.iter().chain(&indeterminate) {
        let before = stored(ids[*i].0).await;
        let s = send(ids[*i].0, ids[*i].1).await;
        assert!(s == 201 || s == 200, "retry of {i}: {s}");
        if before == 1 {
            retried_existing += 1;
        }
    }
    // Exactly once each, in one gap-free canonical order, nothing downgraded.
    assert_eq!(
        scalar(&direct, &format!("SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND message_kind = 'APPLICATION'")).await,
        N as i64
    );
    assert_eq!(
        scalar(&direct, &format!("SELECT count(DISTINCT client_message_id) FROM messaging_messages WHERE conversation_id = '{cid}' AND message_kind = 'APPLICATION'")).await,
        N as i64,
        "no duplicate logical message"
    );
    assert_eq!(
        scalar(&direct, &format!("SELECT count(*) FROM (SELECT sequence, lag(sequence) OVER (ORDER BY sequence) AS prev FROM messaging_messages WHERE conversation_id = '{cid}') s WHERE prev IS NOT NULL AND sequence <> prev + 1")).await,
        0,
        "gap-free sequence"
    );
    assert_eq!(
        scalar(&direct, &format!("SELECT count(*) FROM messaging_conversations WHERE id = '{cid}' AND security_profile = 'MANAGED'")).await,
        1,
        "no security-profile change"
    );
    // The receiver's view after reconnecting: all N, in order.
    let mut seen = Vec::new();
    let mut after = 0i64;
    loop {
        let r = st
            .customer(
                &token,
                Some(bob),
                M::GET,
                &format!("/v1/conversations/{cid}/messages?afterSequence={after}&limit=100"),
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
        let page = r.json["messages"].as_array().cloned().unwrap_or_default();
        for m in &page {
            if m["kind"] == "APPLICATION" {
                seen.push(m["sequence"].as_i64().unwrap());
            }
        }
        match r.json["nextAfterSequence"].as_i64() {
            Some(n) if n > after && !page.is_empty() => after = n,
            _ => break,
        }
    }
    assert_eq!(seen.len(), N, "receiver sees every message once");
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "in canonical order");
    println!(
        "EVIDENCE postgres_loss messages={N} acknowledged_during_run={} refused={} indeterminate={} \
         retries_that_found_a_committed_message={retried_existing} outage_ms={down_ms} total_ms={}",
        acknowledged.len(),
        refused.len(),
        indeterminate.len(),
        started.elapsed().as_millis()
    );
}

// =====================================================================================
// Failure injection — SMS Gateway process death mid-send (real Messaging + Gateway)
// =====================================================================================

#[tokio::test]
async fn sms_gateway_death_mid_send_recovers_without_duplicate_submission() {
    let Some(b) = bins(&["SMS_GATEWAY_BIN", "SMS_GATEWAY_MIGRATION_BIN"]) else {
        return;
    };
    let Some(mut st) = Stack::new(&[]).await else {
        return;
    };
    use cf_service_auth::services as s;
    use reqwest::Method as M;
    let b64 =
        |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let gw_key = key(&st.dir, "gw-key.pem", "sms-gateway-xs-1");
    let callback_secret: [u8; 32] = rand::random();
    std::fs::write(st.dir.join("gw-callback.secret"), b64(&callback_secret)).unwrap();
    std::fs::write(
        st.dir.join("gw-content.key"),
        b64(&rand::random::<[u8; 32]>()),
    )
    .unwrap();
    std::fs::write(
        st.dir.join("gw-fingerprint.key"),
        b64(&rand::random::<[u8; 32]>()),
    )
    .unwrap();
    let gjwk = gw_key.public_jwk();
    let mjwk = st.messaging_key.public_jwk();
    st.messaging_trust(&[(s::SMS_GATEWAY, &gjwk, &["messaging-sms-bridge.deliver"])]);
    std::fs::write(
        st.dir.join("gw-trust.json"),
        trust_bundle(
            s::SMS_GATEWAY,
            &[(s::MESSAGING, &mjwk, &["sms-bridge.submit"])],
        ),
    )
    .unwrap();
    let t = st.tenant("EXTERNAL", &MESSAGING_SCOPES).await;
    let token = st.credential(&t, &MESSAGING_SCOPES).await;
    let owner = st.principal(&t, "EXTERNAL_APPLICATION_PRINCIPAL").await;
    let party = {
        let p = st
            .created(
                &format!("/internal/v1/admin/environments/{}/principals", t.env),
                json!({ "kind": "TELEPHONE_ENDPOINT", "externalReference": Uuid::new_v4().to_string() }),
            )
            .await;
        Uuid::parse_str(p["principal"]["id"].as_str().unwrap()).unwrap()
    };
    let number = Uuid::new_v4();
    let numbering = numbering_stub(NumberingStub {
        number,
        assignment: Uuid::new_v4(),
        scope: json!({ "tenantId": t.tenant, "applicationId": t.app, "environmentId": t.env }),
        app: t.app,
    })
    .await;
    let gw_port = free_port();
    let gw_url = format!("http://127.0.0.1:{gw_port}");
    st.start_messaging(&[("CF_SMS_GATEWAY_BASE_URL", gw_url.clone())])
        .await;
    let gw_db_url = create_db(&st.admin, "xs_sms").await;
    migrate(&b[1], &gw_db_url);
    let gw_db = Database::connect(&gw_db_url).await.unwrap();
    let gw_env = |dispatch_ms: &str| -> Vec<(String, String)> {
        [
            ("DATABASE_URL", gw_db_url.clone()),
            ("SMS_GATEWAY_DATABASE_MAX_CONNECTIONS", "5".into()),
            (
                "SMS_GATEWAY_METRICS_ADDRESS",
                format!("127.0.0.1:{}", free_port()),
            ),
            ("CF_RUNTIME_ENVIRONMENT", env_name()),
            ("CF_SERVICE_AUTH_KEY_ID", "sms-gateway-xs-1".into()),
            ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", st.path("gw-key.pem")),
            ("CF_SERVICE_TRUST_BUNDLE_FILE", st.path("gw-trust.json")),
            ("CF_PLATFORM_ACCESS_BASE_URL", st.pa.url.clone()),
            ("CF_NUMBERING_BASE_URL", numbering.clone()),
            ("CF_MESSAGING_BASE_URL", st.m_url()),
            ("SMS_GATEWAY_CONTENT_KEY_ID", "xs-content-1".into()),
            ("SMS_GATEWAY_CONTENT_KEY_FILE", st.path("gw-content.key")),
            (
                "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
                st.path("gw-fingerprint.key"),
            ),
            ("SMS_GATEWAY_PROVIDER", "deterministic".into()),
            (
                "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
                st.path("gw-callback.secret"),
            ),
            ("SMS_GATEWAY_ALLOWED_DESTINATION_PREFIXES", "+1".into()),
            ("SMS_GATEWAY_WORKER_INTERVAL_MS", dispatch_ms.into()),
            ("SMS_GATEWAY_RETRY_BASE_MS", "100".into()),
            ("SMS_GATEWAY_SUBMIT_TIMEOUT_MS", "300".into()),
            ("RUST_LOG", "sms_gateway=warn".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    };
    // A slow dispatch loop: submissions are accepted (durable) but not yet sent.
    let mut gw = spawn(
        &st.http,
        &b[0],
        "/ready",
        "SMS_GATEWAY_BIND_ADDRESS",
        &gw_env("60000"),
        Some(gw_port),
    )
    .await;
    st.allow_managed(&token, "GROUP").await;
    let conv = st
        .customer(
            &token,
            Some(owner),
            M::POST,
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
        )
        .await;
    let cid = conv.json["conversationId"].as_str().unwrap().to_string();
    let r = st
        .customer(&token, Some(owner), M::POST, &format!("/v1/conversations/{cid}/sms-bridges"),
            Some(json!({ "numberResourceId": number, "to": PARTY, "externalPrincipalId": party,
                "disclosureVersion": messaging::sms_bridge::DISCLOSURE_VERSION, "smsDisclosureAcknowledged": true })))
        .await;
    assert_eq!(r.status, 201, "{}", r.text);

    // (1) The gateway accepts the submission, then dies before dispatching it.
    let m1 = st.send_text(&token, owner, &cid, "before the crash").await;
    assert_eq!(m1.status, 201, "{}", m1.text);
    let mid1 = m1.json["messageId"].as_str().unwrap().to_string();
    let sms1 = eventually(30, "submission accepted by the gateway", || async {
        text(st.mdb(), &format!("SELECT sms_message_id::text FROM messaging_sms_transports WHERE message_id = '{mid1}' AND direction = 'OUTBOUND'")).await
    })
    .await;
    assert_eq!(
        text(
            &gw_db,
            &format!("SELECT status FROM sms_transports WHERE id = '{sms1}'")
        )
        .await
        .as_deref(),
        Some("AUTHORIZED")
    );
    gw.stop().await;
    // (2) Another message while the gateway is down: Messaging keeps it QUEUED (never
    // "failed" or "delivered"); the in-app message is unaffected.
    let m2 = st
        .send_text(&token, owner, &cid, "while the gateway is down")
        .await;
    assert_eq!(
        m2.status, 201,
        "in-app delivery is independent of SMS: {}",
        m2.text
    );
    let mid2 = m2.json["messageId"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    for mid in [&mid1, &mid2] {
        let r = st
            .customer(
                &token,
                Some(owner),
                M::GET,
                &format!("/v1/conversations/{cid}/messages/{mid}/sms-transport"),
                None,
            )
            .await;
        let state = r.json["transportStatus"].as_str().unwrap_or("").to_string();
        assert!(
            !["DELIVERED", "FAILED", "BLOCKED"].contains(&state.as_str()),
            "a dead gateway never yields a final state: {}",
            r.text
        );
    }
    // Restart (fast dispatch): both messages are sent exactly once.
    let _gw = spawn(
        &st.http,
        &b[0],
        "/ready",
        "SMS_GATEWAY_BIND_ADDRESS",
        &gw_env("100"),
        Some(gw_port),
    )
    .await;
    let sms2 = eventually(60, "the queued message reaches the restarted gateway", || async {
        text(st.mdb(), &format!("SELECT sms_message_id::text FROM messaging_sms_transports WHERE message_id = '{mid2}' AND direction = 'OUTBOUND' AND sms_message_id IS NOT NULL")).await
    })
    .await;
    for sms in [&sms1, &sms2] {
        eventually(60, "dispatched after restart", || async {
            (text(
                &gw_db,
                &format!("SELECT status FROM sms_transports WHERE id = '{sms}'"),
            )
            .await
            .as_deref()
                == Some("SUBMITTED"))
            .then_some(())
        })
        .await;
        assert_eq!(
            scalar(
                &gw_db,
                &format!("SELECT count(*) FROM sms_provider_bindings WHERE transport_id = '{sms}'")
            )
            .await,
            1,
            "one provider submission"
        );
    }
    // Messaging's retries while the gateway was down did not create a second transport.
    assert_eq!(
        scalar(
            &gw_db,
            "SELECT count(*) FROM sms_transports WHERE direction = 'OUTBOUND'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar(
            st.mdb(),
            "SELECT count(*) FROM messaging_sms_transports WHERE direction = 'OUTBOUND'"
        )
        .await,
        2
    );
    // Receipts after the restart; a duplicate receipt changes nothing.
    for sms in [&sms1, &sms2] {
        let provider_ref = text(&gw_db, &format!("SELECT provider_message_ref FROM sms_provider_bindings WHERE transport_id = '{sms}'")).await.unwrap();
        let id = format!("rc-{}", Uuid::new_v4().simple());
        let body = json!({ "kind": "RECEIPT", "eventId": id, "messageRef": provider_ref, "status": "DELIVERED",
            "errorCode": null, "costMinor": null, "occurredAt": now_plus(0) });
        let raw = serde_json::to_vec(&body).unwrap();
        for expected in ["ACCEPTED", "DUPLICATE"] {
            let ts = chrono::Utc::now().timestamp().to_string();
            let mut msg = format!("v1:{ts}:{id}:").into_bytes();
            msg.extend_from_slice(&raw);
            let r = send(
                st.http
                    .post(format!(
                        "{gw_url}/provider-callbacks/v1/sms/deterministic/delivery-receipts"
                    ))
                    .header("content-type", "application/json")
                    .header("x-provider-event-id", &id)
                    .header("x-provider-timestamp", ts)
                    .header(
                        "x-provider-signature",
                        format!("v1={}", hex::encode(hmac_sha256(&callback_secret, &msg))),
                    )
                    .body(raw.clone()),
            )
            .await;
            assert_eq!(r.json["status"], expected, "{}", r.text);
        }
    }
    for mid in [&mid1, &mid2] {
        eventually(
            90,
            "DELIVERED reaches Messaging after the restart",
            || async {
                let r = st
                    .customer(
                        &token,
                        Some(owner),
                        M::GET,
                        &format!("/v1/conversations/{cid}/messages/{mid}/sms-transport"),
                        None,
                    )
                    .await;
                (r.json["transportStatus"] == "DELIVERED").then_some(())
            },
        )
        .await;
    }
    assert_eq!(
        scalar(st.mdb(), &format!("SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND message_kind = 'APPLICATION' AND lifecycle = 'ACTIVE'")).await,
        2,
        "the in-app messages stayed consistent"
    );
    println!("EVIDENCE sms_gateway_death submissions=2 provider_submissions_each=1 duplicate_receipts_rejected=2");
}
