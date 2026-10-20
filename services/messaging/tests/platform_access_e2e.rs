//! Part V §10 — A–D FOUNDATION CERTIFICATION across real processes: the REAL Platform
//! Access binary (its own database) and the REAL Messaging binary (its own database),
//! signed internal HTTP between them, real API credentials, real OpenMLS client devices.
//!
//! Requires `TEST_DATABASE_URL`, `PLATFORM_ACCESS_BIN` and `PLATFORM_ACCESS_MIGRATION_BIN`
//! (`make messaging-test`). Without the binaries the journey is SKIPPED with an explicit
//! line, unless `MESSAGING_E2E_REQUIRED=1` (CI), where it fails instead. `pg_dump` /
//! `pg_restore` / `psql` are required for the recovery step under the same rule.
//!
//! This is a foundation certification, NOT an E2EE chat certification. Step 9b adds the
//! Part E realtime handshake on the real binary (client token, single-use ticket,
//! WebSocket, membership-authorized subscription).

mod common;

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use cf_messaging_crypto::device::Device;
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, SystemClock, TokenTtl,
};
use common::{b64, with_db, TestDb};
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, Database, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
/// Deterministic test-only pepper of the test Platform Access (never used outside tests).
const TEST_PEPPER: &str = "messaging-e2e-test-pepper-6f1d0a93c27b48e5af1c";

fn required() -> bool {
    std::env::var("MESSAGING_E2E_REQUIRED").as_deref() == Ok("1")
}

fn skip(what: &str) -> Option<()> {
    if required() {
        panic!("MESSAGING_E2E_REQUIRED=1 but {what}");
    }
    eprintln!("SKIPPED: Platform Access certification journey ({what})");
    None
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_ready(http: &reqwest::Client, url: &str, child: &mut tokio::process::Child) -> bool {
    for _ in 0..400 {
        if let Ok(Some(_)) = child.try_wait() {
            return false;
        }
        if let Ok(r) = http.get(format!("{url}/ready")).send().await {
            if r.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

struct World {
    dir: PathBuf,
    pa_db_url: String,
    http: reqwest::Client,
    pa_url: String,
    pa: tokio::process::Child,
    operator: Arc<ServiceTokenSigner>,
    messaging_bin: PathBuf,
    messaging_env: Vec<(String, String)>,
    /// The key-transparency log key a client pins (out of band).
    kt_public_key: [u8; 32],
}

impl World {
    async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let signed = self
            .operator
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::platform_access(),
            ))
            .unwrap();
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.pa_url))
            .header("x-cf-operator", "messaging-certification");
        for (k, v) in signed.headers().iter() {
            r = r.header(k.as_str(), v.as_bytes());
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn created(&self, path: &str, body: Value) -> Value {
        let (status, v) = self.admin(reqwest::Method::POST, path, Some(body)).await;
        assert!((200..300).contains(&status), "POST {path}: {status} {v}");
        v
    }

    /// A synthetic EXTERNAL tenant / application / SANDBOX environment / service account
    /// whose application is explicitly allowed `granted`.
    async fn tenant(&self, granted: &[&str]) -> (Uuid, Uuid) {
        let slug = format!("t{}", &Uuid::new_v4().simple().to_string()[..12]);
        let t = self
            .created("/internal/v1/admin/tenants", json!({ "slug": slug, "displayName": "messaging certification", "classification": "EXTERNAL" }))
            .await;
        let a = self
            .created(
                &format!(
                    "/internal/v1/admin/tenants/{}/applications",
                    t["id"].as_str().unwrap()
                ),
                json!({ "slug": "app", "displayName": "app" }),
            )
            .await;
        let app = a["id"].as_str().unwrap().to_string();
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
        (env, Uuid::parse_str(sa["id"].as_str().unwrap()).unwrap())
    }

    async fn credential(&self, sa: Uuid, scopes: &[&str]) -> (String, Uuid) {
        let c = self
            .created(
                &format!("/internal/v1/admin/service-accounts/{sa}/credentials"),
                json!({ "scopes": scopes }),
            )
            .await;
        (
            c["secret"].as_str().unwrap().to_string(),
            Uuid::parse_str(c["credential"]["id"].as_str().unwrap()).unwrap(),
        )
    }

    async fn principal(&self, env: Uuid) -> Uuid {
        let p = self
            .created(
                &format!("/internal/v1/admin/environments/{env}/principals"),
                json!({ "kind": "EXTERNAL_APPLICATION_PRINCIPAL", "externalReference": format!("user-{}", Uuid::new_v4().simple()) }),
            )
            .await;
        Uuid::parse_str(p["principal"]["id"].as_str().unwrap()).unwrap()
    }

    async fn start_messaging(&self, db_url: &str) -> (tokio::process::Child, String) {
        for _ in 0..5 {
            let port = free_port();
            let mut c = tokio::process::Command::new(&self.messaging_bin);
            c.env_clear()
                .current_dir(&self.dir)
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("RUST_LOG", "messaging=warn")
                .env("DATABASE_URL", db_url)
                .env("MESSAGING_BIND_ADDRESS", format!("127.0.0.1:{port}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            for (k, v) in &self.messaging_env {
                c.env(k, v);
            }
            let mut child = c.spawn().unwrap();
            let url = format!("http://127.0.0.1:{port}");
            if wait_ready(&self.http, &url, &mut child).await {
                return (child, url);
            }
            let _ = child.kill().await;
        }
        panic!("messaging did not become ready");
    }
}

async fn setup(admin: &str) -> Option<World> {
    let (Some(pa_bin), Some(pa_migration)) = (
        std::env::var_os("PLATFORM_ACCESS_BIN"),
        std::env::var_os("PLATFORM_ACCESS_MIGRATION_BIN"),
    ) else {
        skip("PLATFORM_ACCESS_BIN / PLATFORM_ACCESS_MIGRATION_BIN are not set")?;
        unreachable!()
    };
    let dir = std::env::temp_dir().join(format!("cf-msg-e2e-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    // Platform Access's OWN database, migrated by ITS migration binary.
    let pa_db = format!("msg_t_{}_pa{}", std::process::id(), Uuid::new_v4().simple());
    let conn = Database::connect(admin).await.unwrap();
    conn.execute_unprepared(&format!("CREATE DATABASE {pa_db}"))
        .await
        .unwrap();
    conn.close().await.ok();
    let out = std::process::Command::new(&pa_migration)
        .arg("up")
        .env("DATABASE_URL", with_db(admin, &pa_db))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pa_key = ServiceSigningKey::generate(KeyId::parse("platform-access-test-1").unwrap());
    let messaging_key = ServiceSigningKey::generate(KeyId::parse("messaging-e2e-1").unwrap());
    let operator_key = ServiceSigningKey::generate(KeyId::parse("operator-test-1").unwrap());
    std::fs::write(dir.join("pa-key.pem"), pa_key.to_pkcs8_pem().as_bytes()).unwrap();
    std::fs::write(
        dir.join("messaging-key.pem"),
        messaging_key.to_pkcs8_pem().as_bytes(),
    )
    .unwrap();
    // A SEPARATE Ed25519 key signs key-transparency tree heads (Part V G.5 / §142).
    // Messaging Client Access Token signing key (Part E realtime; a Messaging-only secret).
    let ct_key = ServiceSigningKey::generate(KeyId::parse("messaging-ct-1").unwrap());
    std::fs::write(
        dir.join("messaging-ct.pem"),
        ct_key.to_pkcs8_pem().as_bytes(),
    )
    .unwrap();
    let kt_key = ServiceSigningKey::generate(KeyId::parse("messaging-kt-1").unwrap());
    let kt_pem = kt_key.to_pkcs8_pem();
    std::fs::write(dir.join("messaging-kt.pem"), kt_pem.as_bytes()).unwrap();
    let kt_public_key = {
        use ed25519_dalek::pkcs8::DecodePrivateKey as _;
        ed25519_dalek::SigningKey::from_pkcs8_pem(&kt_pem)
            .unwrap()
            .verifying_key()
            .to_bytes()
    };
    // Platform Access trusts Messaging with EXACTLY its three permissions (ADR 0046).
    std::fs::write(
        dir.join("pa-trust.json"),
        json!({
            "bundle_version": 1, "receiver": cf_service_auth::services::PLATFORM_ACCESS, "runtime_environment": ENV,
            "trusted_issuers": [
                { "issuer": cf_service_auth::services::MESSAGING, "audience": cf_service_auth::services::PLATFORM_ACCESS,
                  "runtime_environment": ENV, "keys": [messaging_key.public_jwk()],
                  "permissions": ["platform-credential.authenticate", "platform-principal.resolve", "platform-identity.read"] },
                { "issuer": cf_service_auth::services::OPERATOR_CONSOLE, "audience": cf_service_auth::services::PLATFORM_ACCESS,
                  "runtime_environment": ENV, "keys": [operator_key.public_jwk()], "permissions": ["platform-identity.administer"] },
            ],
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("messaging-trust.json"),
        json!({ "bundle_version": 1, "receiver": cf_service_auth::services::MESSAGING, "runtime_environment": ENV, "trusted_issuers": [] }).to_string(),
    )
    .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut pa = None;
    for _ in 0..5 {
        let port = free_port();
        let mut child = tokio::process::Command::new(&pa_bin)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("DATABASE_URL", with_db(admin, &pa_db))
            .env("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "3")
            .env("PLATFORM_ACCESS_BIND_ADDRESS", format!("127.0.0.1:{port}"))
            .env("API_CREDENTIAL_PEPPER", TEST_PEPPER)
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CF_SERVICE_AUTH_KEY_ID", "platform-access-test-1")
            .env("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", dir.join("pa-key.pem"))
            .env("CF_SERVICE_TRUST_BUNDLE_FILE", dir.join("pa-trust.json"))
            .env("RUST_LOG", "platform_access=error")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let url = format!("http://127.0.0.1:{port}");
        if wait_ready(&http, &url, &mut child).await {
            pa = Some((child, url));
            break;
        }
        let _ = child.kill().await;
    }
    let (pa, pa_url) = pa.expect("platform access became ready");
    let operator = Arc::new(ServiceTokenSigner::new(
        ServiceId::operator_console(),
        ENV,
        operator_key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let messaging_env = vec![
        ("CF_RUNTIME_ENVIRONMENT".to_string(), "test".to_string()),
        ("CF_SERVICE_AUTH_KEY_ID".into(), "messaging-e2e-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
            dir.join("messaging-key.pem").display().to_string(),
        ),
        (
            "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
            dir.join("messaging-trust.json").display().to_string(),
        ),
        ("CF_PLATFORM_ACCESS_BASE_URL".into(), pa_url.clone()),
        ("MESSAGING_AUTH_CACHE_TTL_SECONDS".into(), "30".into()),
        ("MESSAGING_IDENTITY_FEED_INTERVAL_MS".into(), "250".into()),
        ("MESSAGING_KT_LOG_ID".into(), "cfmeet-messaging:e2e".into()),
        (
            "MESSAGING_KT_SIGNING_KEY_FILE".into(),
            dir.join("messaging-kt.pem").display().to_string(),
        ),
        (
            "MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE".into(),
            dir.join("messaging-ct.pem").display().to_string(),
        ),
    ];
    Some(World {
        dir,
        pa_db_url: with_db(admin, &pa_db),
        http,
        pa_url,
        pa,
        operator,
        messaging_bin: PathBuf::from(env!("CARGO_BIN_EXE_messaging")),
        messaging_env,
        kt_public_key,
    })
}

struct Client<'a> {
    http: &'a reqwest::Client,
    base: String,
}

impl Client<'_> {
    async fn call(
        &self,
        method: reqwest::Method,
        token: &str,
        acting: Option<Uuid>,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> (u16, Value) {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(a) = acting {
            r = r.header("cf-acting-principal", a.to_string());
        }
        if let Some(k) = key {
            r = r.header("idempotency-key", k);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn device(&self, token: &str, principal: Uuid) -> Device {
        let (s, ch) = self
            .call(
                reqwest::Method::POST,
                token,
                Some(principal),
                "/v1/messaging/devices/registration-challenges",
                Some(json!({})),
                None,
            )
            .await;
        assert_eq!(s, 201, "{ch}");
        let device =
            Device::generate(Uuid::parse_str(ch["deviceId"].as_str().unwrap()).unwrap()).unwrap();
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
        let (s, d) = self
            .call(
                reqwest::Method::POST,
                token,
                Some(principal),
                "/v1/messaging/devices",
                Some(json!({ "challengeId": ch["challengeId"], "keyPackage": b64(&kp) })),
                None,
            )
            .await;
        assert_eq!(s, 201, "{d}");
        let (s, _) = self
            .call(
                reqwest::Method::POST,
                token,
                Some(principal),
                &format!("/v1/messaging/devices/{}/key-packages", device.device_id()),
                Some(json!({ "keyPackages": [b64(&device.key_package().unwrap())] })),
                None,
            )
            .await;
        assert_eq!(s, 201);
        device
    }
}

async fn scalar(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        sql.to_string(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index::<i64>(0)
    .unwrap()
}

#[tokio::test]
async fn a_to_d_foundation_certification_across_real_services() {
    let Some(admin) = common::admin_url() else {
        return;
    };
    let Some(mut world) = setup(&admin).await else {
        return;
    };
    // Messaging's OWN database, migrated by ITS migrations.
    let db = TestDb::empty().await.unwrap();
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let (mut messaging, url) = world.start_messaging(&db.url).await;
    let m = Client {
        http: &world.http,
        base: url.clone(),
    };
    use reqwest::Method as M;

    // 1-2. A synthetic external tenant; its credential receives EXPLICITLY approved
    // Messaging scopes and authenticates through Platform Access.
    let all = [
        "messages.send",
        "messages.read",
        "conversations.create",
        "conversations.manage",
    ];
    let (env_a, sa_a) = world.tenant(&all).await;
    let (token, credential) = world.credential(sa_a, &all).await;
    // 4. Authorized communication principals (resolved by Platform Access).
    let alice = world.principal(env_a).await;
    let bob = world.principal(env_a).await;
    // An unknown principal is never authority.
    let (s, body) = m
        .call(
            M::POST,
            &token,
            Some(Uuid::new_v4()),
            "/v1/messaging/devices/registration-challenges",
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(
        (s, body["error"]["reason"].as_str()),
        (403, Some("PRINCIPAL_UNKNOWN"))
    );
    // 5. Trusted device registrations with proof of possession (real OpenMLS devices).
    let alice_device = m.device(&token, alice).await;
    m.device(&token, bob).await;
    // 6. Only PUBLIC device material in Messaging's database.
    assert_eq!(
        scalar(&db.conn, "SELECT count(*) FROM messaging_devices").await,
        2
    );
    let stored_key: Vec<u8> = db
        .conn
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT signature_public_key FROM messaging_devices WHERE id = '{}'",
                alice_device.device_id()
            ),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(stored_key, alice_device.signature_public_key());
    // The device's key-transparency entry is included under a tree head signed by the log
    // key the client pinned (Part V G.5); the head is still UNWITNESSED (AD-05).
    let (s, proofs) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            &format!(
                "/v1/messaging/key-transparency/proofs?deviceId={}",
                alice_device.device_id()
            ),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{proofs}");
    let head = &proofs["treeHead"];
    assert_eq!(head["witnessStatus"], "UNWITNESSED");
    let unb64 = |v: &Value| {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(v.as_str().unwrap())
            .unwrap()
    };
    let root: [u8; 32] = unb64(&head["rootHash"]).try_into().unwrap();
    let input = cf_messaging_crypto::transparency::tree_head_signing_input(
        head["logId"].as_str().unwrap(),
        head["treeSize"].as_u64().unwrap(),
        &root,
        head["timestampMicros"].as_i64().unwrap(),
    );
    assert!(
        cf_messaging_crypto::transparency::verify_tree_head_signature(
            &world.kt_public_key,
            &input,
            &unb64(&head["signature"])
        )
    );
    let entry = &proofs["entries"][0];
    let proof: Vec<[u8; 32]> = entry["inclusionProof"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| unb64(h).try_into().unwrap())
        .collect();
    assert!(cf_messaging_crypto::transparency::verify_inclusion(
        entry["leafIndex"].as_u64().unwrap(),
        head["treeSize"].as_u64().unwrap(),
        &cf_messaging_crypto::transparency::leaf_hash(&unb64(&entry["leafData"])),
        &proof,
        &root
    ));
    // 7-8. A conversation with an approved context and profile, durable in Messaging's DB.
    let (s, conv) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            "/v1/conversations",
            Some(json!({ "contextClass": "DIRECT", "counterpartPrincipalId": bob })),
            Some("cert-direct-0001"),
        )
        .await;
    assert_eq!(s, 201, "{conv}");
    assert_eq!(conv["securityProfile"], "PRIVATE_E2EE");
    let id = conv["conversationId"].as_str().unwrap().to_string();
    assert_eq!(scalar(&db.conn, &format!("SELECT count(*) FROM messaging_conversations WHERE id = '{id}' AND environment_id = '{env_a}'")).await, 1);
    // 9. A second tenant can neither read nor modify it.
    let (_, sa_b) = world.tenant(&all).await;
    let (token_b, _) = world.credential(sa_b, &all).await;
    assert_eq!(
        m.call(
            M::GET,
            &token_b,
            None,
            &format!("/v1/conversations/{id}"),
            None,
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        m.call(
            M::PATCH,
            &token_b,
            None,
            &format!("/v1/conversations/{id}"),
            Some(json!({ "expectedVersion": 1, "lifecycleState": "LOCKED" })),
            None
        )
        .await
        .0,
        404
    );
    // 9b. Part E on the REAL binary: a short-lived client token for Alice's ACTIVE device,
    // a single-use ticket (device-direct), a WebSocket authenticated by it, and a
    // subscription authorized by current membership. The ticket cannot be reused.
    let (s, ct) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            "/v1/messaging/client-tokens",
            Some(json!({ "deviceId": alice_device.device_id() })),
            None,
        )
        .await;
    assert_eq!(s, 201, "{ct}");
    let tk = world
        .http
        .post(format!("{url}/v1/realtime/tickets"))
        .header(
            "authorization",
            format!("CF-Client-Token {}", ct["token"].as_str().unwrap()),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(tk.status().as_u16(), 201);
    let tk: Value = tk.json().await.unwrap();
    let ticket = tk["ticket"].as_str().unwrap().to_string();
    let addr: std::net::SocketAddr = url.trim_start_matches("http://").parse().unwrap();
    let mut ws = common::realtime::open_ws(addr, &ticket, None)
        .await
        .expect("authenticated WebSocket");
    let sub = ws.subscribe(&conv, None).await;
    assert_eq!(sub["type"], "subscribed", "{sub}");
    assert_eq!(
        common::realtime::open_ws(addr, &ticket, None).await.err(),
        Some(401),
        "single-use ticket"
    );
    // The API credential itself is never accepted by the device-direct ticket route.
    let r = world
        .http
        .post(format!("{url}/v1/realtime/tickets"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    drop(ws);
    // 10. Unauthorized scopes cannot administer; new scopes were never granted implicitly.
    let (reader, _) = world.credential(sa_a, &["messages.read"]).await;
    let (s, body) = m
        .call(
            M::PATCH,
            &reader,
            None,
            &format!("/v1/conversations/{id}"),
            Some(json!({ "expectedVersion": 1, "lifecycleState": "LOCKED" })),
            None,
        )
        .await;
    assert_eq!(
        (s, body["error"]["code"].as_str()),
        (403, Some("INSUFFICIENT_SCOPE"))
    );
    // 11. Invalid security-profile transitions fail.
    let (s, body) = m
        .call(
            M::PATCH,
            &token,
            None,
            &format!("/v1/conversations/{id}"),
            Some(json!({ "expectedVersion": 1, "securityProfile": "MANAGED" })),
            None,
        )
        .await;
    assert_eq!(
        (s, body["error"]["code"].as_str()),
        (409, Some("SECURITY_PROFILE_IMMUTABLE"))
    );
    // 12. Concurrent requests preserve uniqueness and versions.
    let mut handles = Vec::new();
    for i in 0..6 {
        let http = world.http.clone();
        let (url, token) = (url.clone(), token.clone());
        handles.push(tokio::spawn(async move {
            let (from, to) = if i % 2 == 0 {
                (alice, bob)
            } else {
                (bob, alice)
            };
            http.post(format!("{url}/v1/conversations"))
                .bearer_auth(&token)
                .header("cf-acting-principal", from.to_string())
                .header("idempotency-key", format!("cert-concurrent-{i}"))
                .json(&json!({ "contextClass": "DIRECT", "counterpartPrincipalId": to }))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()["conversationId"]
                .clone()
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), json!(id));
    }
    let mut lifecycle = Vec::new();
    for v in 1..=3 {
        let target = ["LOCKED", "ACTIVE", "LOCKED"][v - 1];
        let path = format!("/v1/conversations/{id}");
        let body = json!({ "expectedVersion": v, "lifecycleState": target });
        let (a, b) = tokio::join!(
            m.call(M::PATCH, &token, None, &path, Some(body.clone()), None),
            m.call(M::PATCH, &token, None, &path, Some(body.clone()), None),
        );
        let mut statuses = [a.0, b.0];
        statuses.sort();
        assert_eq!(statuses, [200, 409], "exactly one writer wins version {v}");
        lifecycle.push(target);
    }
    // 13. The lifecycle events were committed with their state.
    assert_eq!(
        scalar(&db.conn, &format!("SELECT count(*) FROM messaging_outbox_events WHERE aggregate_id = '{id}' AND event_type = 'messaging.conversation.lifecycle-changed'")).await,
        3
    );
    // Platform Access revocation reaches Messaging: mutations at once, cached reads after
    // the change feed is seen (bounded staleness).
    let (s, _) = m
        .call(
            M::GET,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200);
    world
        .created(
            &format!("/internal/v1/admin/principals/{bob}/status"),
            json!({ "status": "SUSPENDED" }),
        )
        .await;
    let (s, body) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            &format!("/v1/conversations/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, body["error"]["reason"].as_str()),
        (403, Some("PRINCIPAL_INACTIVE"))
    );
    world
        .created(
            &format!("/internal/v1/admin/credentials/{credential}/revoke"),
            json!({}),
        )
        .await;
    let (s, _) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            "/v1/messaging/devices/registration-challenges",
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(s, 401, "a mutation is authenticated afresh");
    let mut revoked_read = 0;
    for _ in 0..40 {
        revoked_read = m
            .call(
                M::GET,
                &token,
                Some(alice),
                &format!("/v1/conversations/{id}"),
                None,
                None,
            )
            .await
            .0;
        if revoked_read == 401 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        revoked_read, 401,
        "the cached read authority was flushed by the change feed"
    );

    // 14. Restart and backup / restore preserve authoritative state.
    let before = scalar(
        &db.conn,
        &format!("SELECT version FROM messaging_conversations WHERE id = '{id}'"),
    )
    .await;
    messaging.start_kill().unwrap();
    let _ = messaging.wait().await;
    let (mut restarted, url2) = world.start_messaging(&db.url).await;
    let ready: Value = world
        .http
        .get(format!("{url2}/ready"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ready["migrations"], "current");
    assert_eq!(
        scalar(
            &db.conn,
            &format!("SELECT version FROM messaging_conversations WHERE id = '{id}'")
        )
        .await,
        before
    );
    restarted.start_kill().unwrap();
    let _ = restarted.wait().await;
    let tools = ["pg_dump", "pg_restore", "psql"].iter().all(|t| {
        std::process::Command::new(t)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    });
    if tools {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dump = world.dir.join("messaging.dump");
        let out = std::process::Command::new(repo.join("infra/deployment/backup/backup.sh"))
            .args(["--service", "messaging", "--database-url", &db.url, "--out"])
            .arg(&dump)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let restored = TestDb::empty().await.unwrap();
        let out = std::process::Command::new(repo.join("infra/deployment/backup/restore.sh"))
            .args(["--database-url", &restored.url, "--in"])
            .arg(&dump)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            messaging_migration::Migrator::get_pending_migrations(&restored.conn)
                .await
                .unwrap()
                .is_empty()
        );
        for table in [
            "messaging_conversations",
            "messaging_devices",
            "messaging_key_packages",
            "messaging_transparency_leaves",
            "messaging_outbox_events",
            "messaging_policy_snapshots",
        ] {
            assert_eq!(
                scalar(&restored.conn, &format!("SELECT count(*) FROM {table}")).await,
                scalar(&db.conn, &format!("SELECT count(*) FROM {table}")).await,
                "{table}"
            );
        }
        let (mut on_restore, url3) = world.start_messaging(&restored.url).await;
        let ready: Value = world
            .http
            .get(format!("{url3}/ready"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(ready["status"], "ready");
        on_restore.start_kill().unwrap();
        let _ = on_restore.wait().await;
    } else {
        skip("pg_dump / pg_restore / psql are not installed (recovery step)");
    }

    // 15. No E2EE private key or plaintext secret entered backend persistence (Messaging's
    // and Platform Access's databases).
    let tables: Vec<String> = db
        .conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public'".to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect();
    let mut dump = String::new();
    for t in tables {
        for r in db
            .conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT to_jsonb(x)::text FROM {t} x"),
            ))
            .await
            .unwrap()
        {
            dump.push_str(&r.try_get_by_index::<String>(0).unwrap());
        }
    }
    // The profile name PRIVATE_E2EE is vocabulary, not material.
    let lower = dump.to_lowercase().replace("private_e2ee", "");
    let dump = dump.replace("PRIVATE_E2EE", "");
    for term in ["private", "secret", "-----begin", &token.to_lowercase()] {
        if let Some(at) = lower.find(term) {
            panic!(
                "{term} in Messaging's database: …{}…",
                &dump[at.saturating_sub(120)..(at + 60).min(dump.len())]
            );
        }
    }
    // Platform Access holds principals and credentials only: no MLS / device-key column.
    let pa = Database::connect(&world.pa_db_url).await.unwrap();
    assert_eq!(
        scalar(&pa, "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' \
                     AND column_name ~ '(mls|key_package|epoch|device|signature_public|private_key)'").await,
        0
    );
    pa.close().await.ok();
    let _ = world.pa.start_kill();
    let _ = world.pa.wait().await;
    if let Ok(conn) = Database::connect(&admin).await {
        let name = world.pa_db_url.rsplit('/').next().unwrap().to_string();
        let _ = conn
            .execute_unprepared(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .await;
    }
    let _ = std::fs::remove_dir_all(&world.dir);
    assert_eq!(lifecycle, ["LOCKED", "ACTIVE", "LOCKED"]);
}

/// A recording stand-in for Calling's platform-event ingress (`POST
/// /internal/v1/platform/events`): what Messaging relays toward customer webhooks. The
/// real Calling path (allow-list projection, signature, delivery) is certified by
/// `cross_service_e2e`; here it shows WHAT leaves Messaging.
async fn webhook_ingress() -> (String, Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::{extract::State, routing::post, Json, Router};
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::default();
    let app = Router::new()
        .route(
            "/internal/v1/platform/events",
            post(
                |State(s): State<Arc<std::sync::Mutex<Vec<Value>>>>, Json(v): Json<Value>| async move {
                    s.lock().unwrap().push(v);
                    axum::http::StatusCode::ACCEPTED
                },
            ),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

/// Part V remediation M11 — the complete EXTERNAL developer journey on real processes:
/// Platform Access (tenant, application, environment, scoped credential, principals),
/// the Messaging binary with real S3-compatible storage (MinIO) and, where provided, the
/// real clamd scanner. Realtime delivery, a MANAGED attachment (upload, finalize,
/// metadata, download grant, server thumbnail), MANAGED search, E2EE search refused
/// (client-local only), relayed lifecycle events without content, member and device
/// revocation, and cross-tenant isolation of every new surface.
#[tokio::test]
async fn external_developer_journey_with_attachments_search_and_webhooks() {
    let Some(admin) = common::admin_url() else {
        return;
    };
    let Ok(endpoint) = std::env::var("OBJECT_STORAGE_TEST_ENDPOINT") else {
        let _ = skip("OBJECT_STORAGE_TEST_ENDPOINT is not set (real S3-compatible storage)");
        return;
    };
    let Some(mut world) = setup(&admin).await else {
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let bucket = format!(
        "{}-e2e",
        var("OBJECT_STORAGE_TEST_BUCKET", "cfmeet-object-storage-test")
    );
    let (access, secret) = (
        var("OBJECT_STORAGE_TEST_ACCESS_KEY", "testing"),
        var("OBJECT_STORAGE_TEST_SECRET_KEY", "testing"),
    );
    let sse = Some(var("OBJECT_STORAGE_TEST_SSE", "AES256")).filter(|s| !s.is_empty());
    cf_object_storage::S3ObjectStore::new(cf_object_storage::S3Config {
        endpoint: endpoint.clone(),
        public_endpoint: None,
        region: "us-east-1".into(),
        bucket: bucket.clone(),
        access_key: access.clone(),
        secret_key: secret.clone(),
        path_style: true,
        server_side_encryption: sse.clone(),
    })
    .unwrap()
    .ensure_bucket()
    .await
    .unwrap();
    let (ingress_url, relayed) = webhook_ingress().await;
    // MANAGED text is sealed at rest under a key ring (a fresh test-only key).
    let keyring = world.dir.join("managed-keyring");
    {
        use base64::Engine as _;
        let key: [u8; 32] = rand::random();
        std::fs::write(
            &keyring,
            format!(
                "e2e-1 {}\n",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
            ),
        )
        .unwrap();
    }
    let clamd = std::env::var("MESSAGING_TEST_CLAMAV_ADDRESS").ok();
    for (k, v) in [
        ("MESSAGING_ATTACHMENTS_S3_ENDPOINT", Some(endpoint)),
        ("MESSAGING_ATTACHMENTS_S3_BUCKET", Some(bucket)),
        ("MESSAGING_ATTACHMENTS_S3_ACCESS_KEY_ID", Some(access)),
        ("MESSAGING_ATTACHMENTS_S3_SECRET_ACCESS_KEY", Some(secret)),
        ("MESSAGING_ATTACHMENTS_S3_PATH_STYLE", Some("true".into())),
        ("MESSAGING_ATTACHMENTS_S3_SERVER_SIDE_ENCRYPTION", sse),
        ("MESSAGING_CLAMAV_ADDRESS", clamd.clone()),
        ("CF_CALLING_BASE_URL", Some(ingress_url)),
        ("MESSAGING_WORKER_INTERVAL_MS", Some("200".into())),
        (
            "MESSAGING_MANAGED_KEYRING_FILE",
            Some(keyring.display().to_string()),
        ),
    ] {
        if let Some(v) = v {
            world.messaging_env.push((k.into(), v));
        }
    }
    let db = TestDb::empty().await.unwrap();
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let (_messaging, url) = world.start_messaging(&db.url).await;
    let m = Client {
        http: &world.http,
        base: url.clone(),
    };
    use reqwest::Method as M;
    let scopes = [
        "messages.send",
        "messages.read",
        "conversations.create",
        "conversations.manage",
        "attachments.write",
        "attachments.read",
    ];
    // 1-3. Synthetic EXTERNAL tenant, application, SANDBOX environment, scoped credential
    // (held by the customer's backend only).
    let (env_a, sa_a) = world.tenant(&scopes).await;
    let (token, _) = world.credential(sa_a, &scopes).await;
    // 4. Principals with trusted devices.
    let alice = world.principal(env_a).await;
    let bob = world.principal(env_a).await;
    let alice_device = m.device(&token, alice).await;
    let bob_device = m.device(&token, bob).await;
    // Policy: MANAGED groups with attachments; scanning required when a scanner runs.
    let (s, p) = m
        .call(M::GET, &token, None, "/v1/messaging/policy", None, None)
        .await;
    assert_eq!(s, 200, "{p}");
    let (s, p) = m
        .call(
            M::PUT,
            &token,
            None,
            "/v1/messaging/policy",
            Some(json!({ "expectedVersion": p["version"], "policy": {
                "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                "attachments": { "enabled": true, "managedScanRequired": clamd.is_some() } } })),
            None,
        )
        .await;
    assert_eq!(s, 200, "{p}");
    // 5. A permitted MANAGED conversation.
    let (s, conv) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            "/v1/conversations",
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
            Some("ext-journey-group-1"),
        )
        .await;
    assert_eq!(s, 201, "{conv}");
    let id = conv["conversationId"].as_str().unwrap().to_string();
    let (s, r) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}/members"),
            Some(json!({ "principalId": bob })),
            None,
        )
        .await;
    assert_eq!(s, 201, "{r}");
    // 6-7. Short-lived client token for Bob's device -> single-use ticket -> WebSocket.
    let (s, ct) = m
        .call(
            M::POST,
            &token,
            Some(bob),
            "/v1/messaging/client-tokens",
            Some(json!({ "deviceId": bob_device.device_id() })),
            None,
        )
        .await;
    assert_eq!(s, 201, "{ct}");
    let tk: Value = world
        .http
        .post(format!("{url}/v1/realtime/tickets"))
        .header(
            "authorization",
            format!("CF-Client-Token {}", ct["token"].as_str().unwrap()),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let addr: std::net::SocketAddr = url.trim_start_matches("http://").parse().unwrap();
    let mut ws = common::realtime::open_ws(addr, tk["ticket"].as_str().unwrap(), None)
        .await
        .expect("bob's WebSocket");
    assert_eq!(ws.subscribe(&conv, None).await["type"], "subscribed");
    // 8-9. Send and receive.
    let note = "quarterly launch plan, see the attached chart";
    let (s, sent) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": note })),
            None,
        )
        .await;
    assert_eq!(s, 201, "{sent}");
    let mut delivered = false;
    for _ in 0..10 {
        let f = ws.next_of("event").await;
        if f["event"]["type"] == "message.created" {
            delivered = true;
            break;
        }
    }
    assert!(delivered, "bob received the message over the WebSocket");
    let (s, list) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            &format!("/v1/conversations/{id}/messages"),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{list}");
    assert!(list.to_string().contains(note));
    // 10-11. An allowed attachment: upload to the real store with the signed grant, finalize.
    let png = {
        let img = image::RgbImage::from_fn(900, 600, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 77])
        });
        let mut b = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png)
            .unwrap();
        b
    };
    let sha = {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(&png))
    };
    let (s, intent) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}/attachments"),
            Some(json!({ "kind": "IMAGE", "size": png.len(), "sha256": sha, "mediaType": "image/png", "fileName": "launch-chart.png" })),
            None,
        )
        .await;
    assert_eq!(s, 201, "{intent}");
    let aid = intent["attachmentId"].as_str().unwrap().to_string();
    let upload = &intent["upload"];
    let mut put = world.http.put(upload["url"].as_str().unwrap());
    for h in upload["headers"].as_array().unwrap() {
        put = put.header(h["name"].as_str().unwrap(), h["value"].as_str().unwrap());
    }
    let r = put.body(png.clone()).send().await.unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let (s, fin) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}/attachments/{aid}/finalize"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(s, 200, "{fin}");
    assert_eq!(fin["state"], "READY", "{fin}");
    let scan = fin["scan"]["status"].as_str().unwrap().to_string();
    assert_eq!(
        scan,
        if clamd.is_some() {
            "CLEAN"
        } else {
            "NOT_REQUIRED"
        },
        "{fin}"
    );
    let (s, r) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "chart attached", "attachmentIds": [aid] })),
            None,
        )
        .await;
    assert_eq!(s, 201, "{r}");
    // 12-13. Bob reads the protected metadata and downloads through a grant.
    let (s, meta) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            &format!("/v1/conversations/{id}/attachments/{aid}"),
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, meta["fileName"].as_str()),
        (200, Some("launch-chart.png")),
        "{meta}"
    );
    let (s, grant) = m
        .call(
            M::POST,
            &token,
            Some(bob),
            &format!("/v1/conversations/{id}/attachments/{aid}/download-grants"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(s, 200, "{grant}");
    let got = world
        .http
        .get(grant["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert!(got.status().is_success());
    assert_eq!(
        got.headers()
            .get("content-disposition")
            .map(|v| v.to_str().unwrap().starts_with("attachment")),
        Some(true),
        "always a download, never rendered inline"
    );
    assert_eq!(got.bytes().await.unwrap().as_ref(), png.as_slice());
    // The server thumbnail (the binary's own lifecycle worker), through its own grant.
    let mut thumb = None;
    for _ in 0..90 {
        let (_, meta) = m
            .call(
                M::GET,
                &token,
                Some(bob),
                &format!("/v1/conversations/{id}/attachments/{aid}"),
                None,
                None,
            )
            .await;
        let t = meta["derivatives"]
            .as_array()
            .and_then(|a| a.iter().find(|d| d["kind"] == "THUMBNAIL").cloned());
        if t.as_ref().is_some_and(|t| t["status"] == "READY") {
            thumb = t;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let thumb = thumb.expect("thumbnail produced by the running service");
    assert_eq!(
        (thumb["width"].as_i64(), thumb["height"].as_i64()),
        (Some(320), Some(213))
    );
    let (s, tg) = m
        .call(
            M::POST,
            &token,
            Some(bob),
            &format!("/v1/conversations/{id}/attachments/{aid}/thumbnail-grants"),
            Some(json!({})),
            None,
        )
        .await;
    assert_eq!(s, 200, "{tg}");
    let tbytes = world
        .http
        .get(tg["url"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(image::load_from_memory(&tbytes).unwrap().width(), 320);
    // 14. MANAGED search within the authorized scope.
    let (s, hits) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            &format!("/v1/conversations/{id}/search?q=quarterly"),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{hits}");
    assert_eq!(hits["results"].as_array().unwrap().len(), 1, "{hits}");
    let (s, across) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            "/v1/messaging/search?q=quarterly",
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, across["results"].as_array().map(|a| a.len())),
        (200, Some(1)),
        "{across}"
    );
    // 15. PRIVATE_E2EE search stays on the devices: the server refuses the query.
    let (s, direct) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            "/v1/conversations",
            Some(json!({ "contextClass": "DIRECT", "counterpartPrincipalId": bob })),
            Some("ext-journey-direct-1"),
        )
        .await;
    assert_eq!(
        (s, direct["securityProfile"].as_str()),
        (201, Some("PRIVATE_E2EE")),
        "{direct}"
    );
    let (s, r) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            &format!(
                "/v1/conversations/{}/search?q=quarterly",
                direct["conversationId"].as_str().unwrap()
            ),
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, r["error"]["code"].as_str()),
        (409, Some("LOCAL_SEARCH_ONLY")),
        "{r}"
    );
    // 16. Lifecycle events reach the platform webhook ingress: ids and states, never
    // content (no message text, file name or bytes of any profile).
    let mut envelopes = Vec::new();
    for _ in 0..60 {
        envelopes = relayed.lock().unwrap().clone();
        if envelopes.iter().any(|e| e.to_string().contains(&id)) && envelopes.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        envelopes.iter().any(|e| e.to_string().contains(&id)),
        "relayed: {envelopes:?}"
    );
    for e in &envelopes {
        let t = e.to_string();
        for leak in [note, "chart attached", "launch-chart.png"] {
            assert!(!t.contains(leak), "content relayed: {t}");
        }
    }
    // 17-18. Revocation: Bob is removed; his next request is refused at once, his
    // subscription is revoked, and his grants are no longer issued.
    let (s, r) = m
        .call(
            M::DELETE,
            &token,
            Some(alice),
            &format!("/v1/conversations/{id}/members/{bob}"),
            None,
            None,
        )
        .await;
    assert!(s == 200 || s == 204, "{s} {r}");
    let revoked = ws.next_of("subscription.revoked").await;
    assert_eq!(revoked["reason"], "MEMBERSHIP_ENDED");
    for (method, path) in [
        (M::GET, format!("/v1/conversations/{id}/messages")),
        (M::GET, format!("/v1/conversations/{id}/attachments/{aid}")),
        (
            M::POST,
            format!("/v1/conversations/{id}/attachments/{aid}/download-grants"),
        ),
        (
            M::POST,
            format!("/v1/conversations/{id}/attachments/{aid}/thumbnail-grants"),
        ),
        (M::GET, format!("/v1/conversations/{id}/search?q=quarterly")),
    ] {
        let body = (method == M::POST).then(|| json!({}));
        let (s, r) = m
            .call(method.clone(), &token, Some(bob), &path, body, None)
            .await;
        assert!(s == 403 || s == 404, "{method} {path}: {s} {r}");
        assert!(
            !r.to_string().contains(note) && !r.to_string().contains("launch-chart"),
            "{r}"
        );
    }
    let (_, across) = m
        .call(
            M::GET,
            &token,
            Some(bob),
            "/v1/messaging/search?q=quarterly",
            None,
            None,
        )
        .await;
    assert_eq!(
        across["results"].as_array().map(|a| a.len()),
        Some(0),
        "{across}"
    );
    // Device revocation: Alice's device gets no new client token.
    let (s, r) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            &format!("/v1/messaging/devices/{}/revoke", alice_device.device_id()),
            Some(json!({ "reason": "LOST_OR_STOLEN" })),
            None,
        )
        .await;
    assert!((200..300).contains(&s), "{s} {r}");
    let (s, r) = m
        .call(
            M::POST,
            &token,
            Some(alice),
            "/v1/messaging/client-tokens",
            Some(json!({ "deviceId": alice_device.device_id() })),
            None,
        )
        .await;
    assert!(s == 403 || s == 409, "{s} {r}");
    // 19-20. Another tenant reaches none of it.
    let (_, sa_b) = world.tenant(&scopes).await;
    let (token_b, _) = world.credential(sa_b, &scopes).await;
    for (method, path) in [
        (M::GET, format!("/v1/conversations/{id}")),
        (M::GET, format!("/v1/conversations/{id}/attachments/{aid}")),
        (
            M::POST,
            format!("/v1/conversations/{id}/attachments/{aid}/download-grants"),
        ),
        (
            M::POST,
            format!("/v1/conversations/{id}/attachments/{aid}/thumbnail-grants"),
        ),
        (M::GET, format!("/v1/conversations/{id}/search?q=quarterly")),
    ] {
        let body = (method == M::POST).then(|| json!({}));
        let (s, r) = m
            .call(method.clone(), &token_b, Some(alice), &path, body, None)
            .await;
        assert!(
            s == 403 || s == 404,
            "cross-tenant {method} {path}: {s} {r}"
        );
    }
    println!(
        "EVIDENCE external_journey real_platform_access=1 real_messaging_binary=1 real_s3=1 scanner={} relayed_events={} thumbnail=READY",
        if clamd.is_some() { "clamd" } else { "not-required-by-policy" },
        envelopes.len()
    );
}
