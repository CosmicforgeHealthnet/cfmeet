//! Part V H — Meeting chat migration and the CF Meet identity bridge across REAL
//! service processes (mandatory end-to-end scenarios 1, 2, 6 and 7).
//!
//! * The REAL Platform Access binary (its own database, its own migrations).
//! * The REAL Messaging binary (its own database, its own migrations, MANAGED key ring).
//! * The real Meeting Service router and integration in this process (its own database),
//!   talking to both over signed internal HTTP (`cfmeet.meeting-service` service tokens
//!   with CF Meet's FIRST_PARTY delegated scope). No database is shared or read across
//!   services: every Messaging check below reads Messaging's OWN database only as the
//!   test's evidence, never through Meeting.
//! * A real OpenMLS client device (keys generated in the test "client").
//!
//! Requires `TEST_DATABASE_URL`, `PLATFORM_ACCESS_BIN`, `PLATFORM_ACCESS_MIGRATION_BIN`,
//! `MESSAGING_BIN` and `MESSAGING_MIGRATION_BIN`. Without the binaries the journey is
//! SKIPPED with an explicit line, unless `UNIFIED_MESSAGING_E2E_REQUIRED=1` (CI), where it
//! fails instead.

mod common;

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use axum::http::StatusCode;
use backend::{
    config::FeatureFlags,
    services::unified_messaging::{
        config::{CommunicationScope, UnifiedMessagingConfig},
        gateway::ServiceAuthGateway,
        UnifiedMessaging,
    },
};
use base64::Engine;
use cf_service_auth::{
    ApplicationEnvironmentKind, KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId,
    ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl,
};
use common::{collab, uuid, Auth, RtcMode, TestApp, TestOptions};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
const TEST_PEPPER: &str = "meeting-um-e2e-test-pepper-3c9a51f0d2e64b7a";
const MEETING_KID: &str = "meeting-um-e2e-1";

fn required() -> bool {
    std::env::var("UNIFIED_MESSAGING_E2E_REQUIRED").as_deref() == Ok("1")
}

fn binaries() -> Option<[PathBuf; 4]> {
    let get = |k: &str| std::env::var_os(k).map(PathBuf::from);
    match (
        get("PLATFORM_ACCESS_BIN"),
        get("PLATFORM_ACCESS_MIGRATION_BIN"),
        get("MESSAGING_BIN"),
        get("MESSAGING_MIGRATION_BIN"),
    ) {
        (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
        _ => {
            if required() {
                panic!("UNIFIED_MESSAGING_E2E_REQUIRED=1 but the service binaries are not set");
            }
            eprintln!(
                "SKIPPED: unified messaging journey (PLATFORM_ACCESS_BIN / MESSAGING_BIN not set)"
            );
            None
        }
    }
}

/// `UM_E2E_LOGS=1` shows the services' own logs (diagnosis).
fn stderr() -> Stdio {
    if std::env::var("UM_E2E_LOGS").as_deref() == Ok("1") {
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

fn with_db(admin: &str, db: &str) -> String {
    let mut u = url::Url::parse(admin).unwrap();
    u.set_path(&format!("/{db}"));
    u.to_string()
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

async fn create_db(admin: &str, prefix: &str) -> String {
    let name = format!("{prefix}_{}", Uuid::new_v4().simple());
    let conn = Database::connect(admin).await.unwrap();
    conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    conn.close().await.ok();
    with_db(admin, &name)
}

fn migrate(bin: &PathBuf, url: &str) {
    let out = std::process::Command::new(bin)
        .arg("up")
        .env("DATABASE_URL", url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct World {
    dir: PathBuf,
    http: reqwest::Client,
    pa_url: String,
    _pa: tokio::process::Child,
    operator: Arc<ServiceTokenSigner>,
    messaging_bin: PathBuf,
    messaging_db_url: String,
    messaging_env: Vec<(String, String)>,
    messaging: Option<tokio::process::Child>,
    messaging_url: String,
    messaging_db: DatabaseConnection,
    scope: CommunicationScope,
    /// CF Meet's own backend API credential (policy administration only).
    credential: String,
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
            .header("x-cf-operator", "meeting-um-certification");
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

    async fn start_messaging(&mut self) {
        for _ in 0..5 {
            let port = free_port();
            let mut c = tokio::process::Command::new(&self.messaging_bin);
            c.env_clear()
                .current_dir(&self.dir)
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("RUST_LOG", "messaging=warn")
                .env("DATABASE_URL", &self.messaging_db_url)
                .env("MESSAGING_BIND_ADDRESS", format!("127.0.0.1:{port}"))
                .stdout(Stdio::null())
                .stderr(stderr())
                .kill_on_drop(true);
            for (k, v) in &self.messaging_env {
                c.env(k, v);
            }
            let mut child = c.spawn().unwrap();
            let url = format!("http://127.0.0.1:{port}");
            if wait_ready(&self.http, &url, &mut child).await {
                // Keep the address stable across restarts (the gateway is configured once).
                if self.messaging_url.is_empty() || self.messaging_url == url {
                    self.messaging_url = url;
                    self.messaging = Some(child);
                    return;
                }
            }
            let _ = child.kill().await;
        }
        panic!("messaging did not become ready");
    }

    /// Restart on the SAME port (the Meeting gateway keeps its configured base URL).
    async fn restart_messaging(&mut self) {
        let port: u16 = self
            .messaging_url
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        for _ in 0..50 {
            let mut c = tokio::process::Command::new(&self.messaging_bin);
            c.env_clear()
                .current_dir(&self.dir)
                .env("PATH", std::env::var("PATH").unwrap_or_default())
                .env("RUST_LOG", "messaging=warn")
                .env("DATABASE_URL", &self.messaging_db_url)
                .env("MESSAGING_BIND_ADDRESS", format!("127.0.0.1:{port}"))
                .stdout(Stdio::null())
                .stderr(stderr())
                .kill_on_drop(true);
            for (k, v) in &self.messaging_env {
                c.env(k, v);
            }
            let mut child = c.spawn().unwrap();
            if wait_ready(&self.http, &self.messaging_url.clone(), &mut child).await {
                self.messaging = Some(child);
                return;
            }
            let _ = child.kill().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("messaging did not restart");
    }

    async fn stop_messaging(&mut self) {
        if let Some(mut c) = self.messaging.take() {
            c.kill().await.unwrap();
            let _ = c.wait().await;
        }
    }

    async fn customer(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.messaging_url))
            .bearer_auth(&self.credential);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn m_scalar(&self, sql: &str) -> i64 {
        self.messaging_db
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

    fn gateway_config(&self) -> UnifiedMessagingConfig {
        UnifiedMessagingConfig {
            runtime: ENV,
            key_id: KeyId::parse(MEETING_KID).unwrap(),
            key_file: self.dir.join("meeting-key.pem"),
            token_ttl_seconds: 120,
            timeout: Duration::from_secs(5),
            platform_access_url: self.pa_url.clone(),
            messaging_url: self.messaging_url.clone(),
            scope: self.scope,
            sync_interval: Duration::from_millis(500),
        }
    }
}

async fn setup() -> Option<World> {
    let admin = std::env::var("TEST_DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())?;
    let [pa_bin, pa_migration, messaging_bin, messaging_migration] = binaries()?;
    let dir = std::env::temp_dir().join(format!("cf-meeting-um-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let pa_db = create_db(&admin, "um_pa").await;
    migrate(&pa_migration, &pa_db);
    let messaging_db_url = create_db(&admin, "um_msg").await;
    migrate(&messaging_migration, &messaging_db_url);

    let gen = |kid: &str| ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let pa_key = gen("pa-um-1");
    let messaging_key = gen("messaging-um-1");
    let meeting_key = gen(MEETING_KID);
    let operator_key = gen("operator-um-1");
    let write = |name: &str, k: &ServiceSigningKey| {
        std::fs::write(dir.join(name), k.to_pkcs8_pem().as_bytes()).unwrap()
    };
    write("pa-key.pem", &pa_key);
    write("messaging-key.pem", &messaging_key);
    write("meeting-key.pem", &meeting_key);
    write("messaging-ct.pem", &gen("messaging-ct-um"));
    write("messaging-kt.pem", &gen("messaging-kt-um"));
    let ring_key =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    std::fs::write(dir.join("managed.ring"), format!("um-1 {ring_key}\n")).unwrap();
    use cf_service_auth::services as s;
    // Receiver-authoritative trust: Platform Access grants Meeting ONLY principal ensure.
    std::fs::write(
        dir.join("pa-trust.json"),
        json!({
            "bundle_version": 1, "receiver": s::PLATFORM_ACCESS, "runtime_environment": ENV,
            "trusted_issuers": [
                { "issuer": s::MESSAGING, "audience": s::PLATFORM_ACCESS, "runtime_environment": ENV,
                  "keys": [messaging_key.public_jwk()],
                  "permissions": ["platform-credential.authenticate", "platform-principal.resolve", "platform-identity.read"] },
                { "issuer": s::MEETING_SERVICE, "audience": s::PLATFORM_ACCESS, "runtime_environment": ENV,
                  "keys": [meeting_key.public_jwk()], "permissions": ["platform-principal.ensure"] },
                { "issuer": s::OPERATOR_CONSOLE, "audience": s::PLATFORM_ACCESS, "runtime_environment": ENV,
                  "keys": [operator_key.public_jwk()], "permissions": ["platform-identity.administer"] },
            ],
        })
        .to_string(),
    )
    .unwrap();
    // Messaging grants Meeting exactly its two integration permissions.
    std::fs::write(
        dir.join("messaging-trust.json"),
        json!({
            "bundle_version": 1, "receiver": s::MESSAGING, "runtime_environment": ENV,
            "trusted_issuers": [
                { "issuer": s::MEETING_SERVICE, "audience": s::MESSAGING, "runtime_environment": ENV,
                  "keys": [meeting_key.public_jwk()],
                  "permissions": ["messaging-application.delegate", "messaging-meeting.sync"] },
            ],
        })
        .to_string(),
    )
    .unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut pa = None;
    for _ in 0..5 {
        let port = free_port();
        let mut child = tokio::process::Command::new(&pa_bin)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("DATABASE_URL", &pa_db)
            .env("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "3")
            .env("PLATFORM_ACCESS_BIND_ADDRESS", format!("127.0.0.1:{port}"))
            .env("API_CREDENTIAL_PEPPER", TEST_PEPPER)
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CF_SERVICE_AUTH_KEY_ID", "pa-um-1")
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
    let p = |name: &str| dir.join(name).display().to_string();
    let messaging_env = vec![
        ("CF_RUNTIME_ENVIRONMENT".to_string(), "test".to_string()),
        ("CF_SERVICE_AUTH_KEY_ID".into(), "messaging-um-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
            p("messaging-key.pem"),
        ),
        (
            "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
            p("messaging-trust.json"),
        ),
        ("CF_PLATFORM_ACCESS_BASE_URL".into(), pa_url.clone()),
        ("MESSAGING_AUTH_CACHE_TTL_SECONDS".into(), "30".into()),
        ("MESSAGING_IDENTITY_FEED_INTERVAL_MS".into(), "250".into()),
        (
            "MESSAGING_KT_LOG_ID".into(),
            "cfmeet-messaging:um-e2e".into(),
        ),
        (
            "MESSAGING_KT_SIGNING_KEY_FILE".into(),
            p("messaging-kt.pem"),
        ),
        (
            "MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE".into(),
            p("messaging-ct.pem"),
        ),
        ("MESSAGING_MANAGED_KEYRING_FILE".into(), p("managed.ring")),
    ];
    let messaging_db = Database::connect(&messaging_db_url).await.unwrap();
    let mut world = World {
        dir,
        http,
        pa_url,
        _pa: pa,
        operator,
        messaging_bin,
        messaging_db_url,
        messaging_env,
        messaging: None,
        messaging_url: String::new(),
        messaging_db,
        scope: CommunicationScope {
            tenant_id: Uuid::nil(),
            application_id: Uuid::nil(),
            environment_id: Uuid::nil(),
            kind: ApplicationEnvironmentKind::Sandbox,
            service_account_id: Uuid::nil(),
        },
        credential: String::new(),
    };
    // CF Meet's FIRST_PARTY tenant / application / SANDBOX environment / service account.
    let all = [
        "messages.send",
        "messages.read",
        "conversations.create",
        "conversations.manage",
    ];
    let slug = format!("cfmeet{}", &Uuid::new_v4().simple().to_string()[..10]);
    let t = world
        .created(
            "/internal/v1/admin/tenants",
            json!({ "slug": slug, "displayName": "CF Meet", "classification": "FIRST_PARTY" }),
        )
        .await;
    let tenant = uuid(&t["id"]);
    let a = world
        .created(
            &format!("/internal/v1/admin/tenants/{tenant}/applications"),
            json!({ "slug": "cfmeet", "displayName": "CF Meet" }),
        )
        .await;
    let app = uuid(&a["id"]);
    for scope in all {
        world
            .created(
                &format!("/internal/v1/admin/applications/{app}/scopes"),
                json!({ "scope": scope }),
            )
            .await;
    }
    let e = world
        .created(
            &format!("/internal/v1/admin/applications/{app}/environments"),
            json!({ "kind": "SANDBOX" }),
        )
        .await;
    let env = uuid(&e["id"]);
    let sa = world
        .created(
            &format!("/internal/v1/admin/environments/{env}/service-accounts"),
            json!({ "name": "meeting-backend" }),
        )
        .await;
    let sa = uuid(&sa["id"]);
    let c = world
        .created(
            &format!("/internal/v1/admin/service-accounts/{sa}/credentials"),
            json!({ "scopes": all }),
        )
        .await;
    world.credential = c["secret"].as_str().unwrap().to_string();
    world.scope = CommunicationScope {
        tenant_id: tenant,
        application_id: app,
        environment_id: env,
        kind: ApplicationEnvironmentKind::Sandbox,
        service_account_id: sa,
    };
    world.start_messaging().await;
    // Meeting chat is MANAGED (server-readable, disclosed); never relabelled as E2EE.
    let (s, current) = world
        .customer(reqwest::Method::GET, "/v1/messaging/policy", None)
        .await;
    assert_eq!(s, 200, "{current}");
    let (s, v) = world
        .customer(
            reqwest::Method::PUT,
            "/v1/messaging/policy",
            Some(json!({ "expectedVersion": current["version"], "policy": { "contexts": {
                "MEETING": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } } })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    Some(world)
}

async fn meeting_app(world: &World) -> TestApp {
    let gateway = ServiceAuthGateway::new(&world.gateway_config()).unwrap();
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: FeatureFlags {
            rtc_provider_control_v2: true,
            conference_realtime_enabled: true,
            collaboration_v2_enabled: true,
            ..FeatureFlags::all_v2_enabled()
        },
        unified_messaging: Some(Arc::new(gateway)),
        ..TestOptions::default()
    })
    .await
    .unwrap()
}

fn um(app: &TestApp) -> Arc<UnifiedMessaging> {
    app.state
        .meeting
        .core
        .unified
        .clone()
        .expect("integration configured")
}

async fn transition(
    app: &TestApp,
    admin: &common::User,
    target: &str,
    approved_by: Option<&str>,
) -> (StatusCode, Value) {
    let (_, status) = app
        .get("/api/v2/admin/chat-migration", Auth::Bearer(&admin.token))
        .await;
    let mut body = json!({ "targetState": target, "expectedVersion": status["version"], "reason": format!("certification step to {target}") });
    if let Some(a) = approved_by {
        body["approvedBy"] = json!(a);
    }
    app.post(
        "/api/v2/admin/chat-migration/transitions",
        Auth::Bearer(&admin.token),
        body,
    )
    .await
}

#[tokio::test]
async fn meeting_chat_migrates_to_unified_messaging_across_real_services() {
    let Some(mut world) = setup().await else {
        return;
    };
    let app = meeting_app(&world).await;
    let um = um(&app);
    let ops = app.admin("ops").await;
    let (owner, cid, guests) = collab::meeting(&app, collab::public(), 2).await;
    let host = collab::owner_joined(&app, cid, &owner).await;
    let (g0, g1) = (&guests[0], &guests[1]);
    let base = collab::base(cid);

    // ---- LEGACY_PRIMARY: chat works exactly as before; Messaging is not involved.
    let (s, hello) =
        collab::send(&app, cid, g0.auth(), json!({ "body": "hello from legacy" })).await;
    assert_eq!(s, StatusCode::CREATED, "{hello}");
    let (s, welcome) = collab::send(
        &app,
        cid,
        Auth::Bearer(&owner.token),
        json!({ "body": "Welcome!", "announcement": true }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{welcome}");
    let (_, doomed) = collab::send(&app, cid, g1.auth(), json!({ "body": "to be deleted" })).await;
    let (s, v) = app
        .delete(
            &format!("{base}/messages/{}", doomed["id"].as_str().unwrap()),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let r = um.sync_once().await.unwrap();
    assert_eq!(
        r.conversations_linked, 0,
        "LEGACY_PRIMARY never touches Messaging"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM conference_chat_links")
            .await,
        0
    );

    // Only platform administrators drive the migration.
    let (s, _) = app
        .get("/api/v2/admin/chat-migration", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = transition(&app, &ops, "MESSAGING_PRIMARY", Some("second-operator")).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("CHAT_MIGRATION_REFUSED")),
        "no shortcut: {v}"
    );

    // ---- SHADOW_VALIDATION: conversation, members and LEGACY_MANAGED history mirrored.
    let (s, v) = transition(&app, &ops, "SHADOW_VALIDATION", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let r = um.sync_once().await.unwrap();
    assert_eq!(r.failures, 0, "{r:?}");
    assert_eq!(r.conversations_linked, 1, "{r:?}");
    assert_eq!(r.messages_mirrored, 3, "{r:?}");
    // Deleted before mirroring: imported directly as a content-free tombstone.
    assert_eq!(r.deletions_mirrored, 0, "{r:?}");
    let conversation = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_chat_links WHERE conference_id = '{cid}'"
        ))
        .await;
    assert_eq!(conversation, 1);
    assert_eq!(
        world.m_scalar("SELECT count(*) FROM messaging_conversations WHERE context_class = 'MEETING' AND security_profile = 'MANAGED'").await,
        1
    );
    assert_eq!(
        world.m_scalar("SELECT count(*) FROM messaging_messages WHERE security_provenance = 'LEGACY_MANAGED' AND original_created_at IS NOT NULL AND legacy_source = 'meeting.conference-messages'").await,
        3
    );
    assert_eq!(
        world.m_scalar("SELECT count(*) FROM messaging_messages WHERE lifecycle = 'DELETED' AND lifecycle_reason = 'LEGACY_DELETED' AND managed_sealed IS NULL").await,
        1
    );
    assert_eq!(world.m_scalar("SELECT count(*) FROM messaging_messages WHERE managed_presentation = 'ANNOUNCEMENT'").await, 1);
    // MANAGED text is sealed at rest in Messaging (never stored as plaintext).
    assert_eq!(
        world.m_scalar("SELECT count(*) FROM messaging_messages WHERE position(convert_to('hello from legacy', 'UTF8') in managed_sealed) > 0").await,
        0
    );
    // Host + two guests are members; the owner holds OWNER, guests MEMBER.
    assert_eq!(
        world
            .m_scalar("SELECT count(*) FROM messaging_conversation_members WHERE state = 'ACTIVE'")
            .await,
        3
    );
    // Shadow mode: legacy stays authoritative for new messages.
    let (s, _) = collab::send(&app, cid, g0.auth(), json!({ "body": "during shadow" })).await;
    assert_eq!(s, StatusCode::CREATED);
    // A deletion after mirroring becomes a moderator removal of the canonical message.
    let (_, oops) = collab::send(&app, cid, g0.auth(), json!({ "body": "oops" })).await;
    um.sync_once().await.unwrap();
    let (s, _) = app
        .delete(
            &format!("{base}/messages/{}", oops["id"].as_str().unwrap()),
            g0.auth(),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let r = um.sync_once().await.unwrap();
    assert_eq!(r.deletions_mirrored, 1, "{r:?}");
    assert_eq!(world.m_scalar("SELECT count(*) FROM messaging_messages WHERE lifecycle = 'MODERATED' AND lifecycle_reason = 'LEGACY_DELETED'").await, 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_messages WHERE conference_id = '{cid}'"
        ))
        .await,
        5
    );
    let (s, v) = transition(&app, &ops, "MESSAGING_PRIMARY", Some("second-operator")).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "pending mirror / no parity must block: {v}"
    );
    um.sync_once().await.unwrap();
    let (s, parity) = app
        .post(
            "/api/v2/admin/chat-migration/parity-runs",
            Auth::Bearer(&ops.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{parity}");
    assert_eq!(parity["status"], "PASSED", "{parity}");
    assert_eq!(parity["messagesChecked"], 5, "{parity}");

    // ---- Cutover gates: a second named approver who is not the requester.
    let (s, v) = transition(&app, &ops, "MESSAGING_PRIMARY", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let me = format!("user:{}", ops.id);
    let (s, v) = transition(&app, &ops, "MESSAGING_PRIMARY", Some(&me)).await;
    assert_eq!(s, StatusCode::CONFLICT, "self-approval: {v}");
    let (s, v) = app
        .post("/api/v2/admin/chat-migration/transitions", Auth::Bearer(&ops.token),
            json!({ "targetState": "MESSAGING_PRIMARY", "expectedVersion": 1, "reason": "stale view", "approvedBy": "second-operator" }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "stale version: {v}");
    let (s, v) = transition(&app, &ops, "MESSAGING_PRIMARY", Some("second-operator")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "MESSAGING_PRIMARY");

    // ---- MESSAGING_PRIMARY: the compatibility adapter; Messaging is the authority.
    let legacy_rows = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_messages WHERE conference_id = '{cid}'"
        ))
        .await;
    let (s, native) = collab::send(
        &app,
        cid,
        g1.auth(),
        json!({ "body": "native after cutover" }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{native}");
    assert_eq!(native["body"], "native after cutover");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_messages WHERE conference_id = '{cid}'"
        ))
        .await,
        legacy_rows,
        "no new authoritative legacy write after cutover"
    );
    assert_eq!(
        world
            .m_scalar(
                "SELECT count(*) FROM messaging_messages WHERE security_provenance = 'NATIVE'"
            )
            .await,
        1
    );
    // The database itself refuses a main-room legacy insert now.
    let sender = g0.participant_id;
    let refused = app
        .db
        .conn
        .execute_unprepared(&format!(
            "INSERT INTO conference_messages (id, conference_id, sender_participant_id, body) VALUES ('{}', '{cid}', '{sender}', 'sneaky')",
            Uuid::new_v4()
        ))
        .await;
    assert!(refused.unwrap_err().to_string().contains("CHAT_MIGRATED"));
    // Reads: one ordered history, legacy ids kept, deleted content withheld.
    let (s, page) = collab::messages(&app, cid, g0.auth(), "").await;
    assert_eq!(s, StatusCode::OK, "{page}");
    let msgs = page["messages"].as_array().unwrap();
    let bodies: Vec<Option<&str>> = msgs.iter().map(|m| m["body"].as_str()).collect();
    assert_eq!(
        bodies,
        vec![
            Some("native after cutover"),
            None,
            Some("during shadow"),
            None,
            Some("Welcome!"),
            Some("hello from legacy")
        ],
        "{page}"
    );
    assert_eq!(
        msgs[5]["id"], hello["id"],
        "legacy ids survive the migration"
    );
    assert_eq!(msgs[4]["kind"], "ANNOUNCEMENT");
    assert_eq!(msgs[3]["deleted"], true);
    assert_eq!(msgs[1]["deleted"], true);
    assert_eq!(msgs[0]["senderParticipantId"], json!(g1.participant_id));
    // Pagination continues across native and imported messages.
    let (_, p1) = collab::messages(&app, cid, g0.auth(), "?limit=2").await;
    let cursor = p1["nextCursor"].as_str().unwrap().to_string();
    let (_, p2) =
        collab::messages(&app, cid, g0.auth(), &format!("?limit=2&before={cursor}")).await;
    assert_eq!(p2["messages"][0]["body"], "during shadow", "{p2}");
    assert_eq!(p2["messages"][1]["body"], json!(null), "{p2}");
    // Moderation still follows meeting roles; the removal lands in Messaging.
    let (s, v) = app
        .delete(
            &format!("{base}/messages/{}", native["id"].as_str().unwrap()),
            g0.auth(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a guest cannot moderate: {v}");
    let (s, v) = app
        .delete(
            &format!("{base}/messages/{}", native["id"].as_str().unwrap()),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(
        (s, v["changed"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{v}"
    );
    assert_eq!(world.m_scalar("SELECT count(*) FROM messaging_messages WHERE lifecycle = 'MODERATED' AND lifecycle_reason = 'MEETING_CHAT_DELETED'").await, 1);
    // Membership follows meeting admission (removal is applied, version-fenced).
    let (s, v) = app
        .post(
            &format!("{base}/participants/{}/remove", g1.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let r = um.sync_once().await.unwrap();
    assert!(r.members_synced >= 1, "{r:?}");
    assert_eq!(
        world
            .m_scalar("SELECT count(*) FROM messaging_conversation_members WHERE state = 'REMOVED'")
            .await,
        1
    );

    // ---- Scenario 2: native identity — CF Meet login → principal → device → client token.
    let bridge = "/api/v2/messaging";
    let (s, ch) = app
        .post(
            &format!("{bridge}/devices/registration-challenges"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{ch}");
    let device = cf_messaging_crypto::device::Device::generate(uuid(&ch["deviceId"])).unwrap();
    let nonce: [u8; 32] = hex::decode(ch["nonce"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let kp = device
        .registration_key_package(uuid(&ch["challengeId"]), &nonce)
        .unwrap();
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let (s, d) = app
        .post(
            &format!("{bridge}/devices"),
            Auth::Bearer(&owner.token),
            json!({ "challengeId": ch["challengeId"], "keyPackage": b64(&kp) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    let (s, t) = app
        .post(
            &format!("{bridge}/client-tokens"),
            Auth::Bearer(&owner.token),
            json!({ "deviceId": device.device_id() }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    assert_eq!(t["deviceId"], json!(device.device_id()));
    assert_eq!(t["token"].as_str().unwrap().split('.').count(), 3);
    let text = t.to_string();
    for forbidden in [
        MEETING_KID,
        "x-cf-service-authorization",
        world.credential.as_str(),
    ] {
        assert!(
            !text.contains(forbidden),
            "no service credential reaches the client"
        );
    }
    // The principal is the account's NATIVE_CF_USER principal (opaque reference).
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM communication_principal_links WHERE subject_kind = 'USER' AND subject_id = '{}' AND principal_kind = 'NATIVE_CF_USER'", owner.id)).await,
        1
    );
    // Guests have no account: the bridge refuses them.
    let (s, _) = app
        .post(
            &format!("{bridge}/client-tokens"),
            g0.auth(),
            json!({ "deviceId": device.device_id() }),
        )
        .await;
    assert!(s.is_client_error());
    // Another account cannot use this device.
    let other = app.user("other").await;
    let (s, v) = app
        .post(
            &format!("{bridge}/client-tokens"),
            Auth::Bearer(&other.token),
            json!({ "deviceId": device.device_id() }),
        )
        .await;
    assert!(s.is_client_error(), "{v}");
    // Logout ends every Messaging session of the account; the login is then refused.
    const ACTIVE: &str = "SELECT count(*) FROM messaging_client_sessions WHERE status = 'ACTIVE'";
    assert_eq!(world.m_scalar(ACTIVE).await, 1);
    let (s, v) = app
        .post("/api/v1/auth/logout", Auth::Bearer(&owner.token), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        world.m_scalar(ACTIVE).await,
        0,
        "logout revoked the Messaging session"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM communication_session_revocations WHERE status = 'DONE'")
            .await,
        1
    );
    let (s, _) = app
        .post(
            &format!("{bridge}/client-tokens"),
            Auth::Bearer(&owner.token),
            json!({ "deviceId": device.device_id() }),
        )
        .await;
    assert!(s.is_client_error(), "a logged-out token is refused");

    // ---- Scenario 7: failure recovery. Messaging down → chat 503, the meeting goes on.
    world.stop_messaging().await;
    let (s, v) = collab::send(
        &app,
        cid,
        g0.auth(),
        json!({ "body": "while messaging is down" }),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("CHAT_UNAVAILABLE")),
        "{v}"
    );
    let (s, _) = app
        .get(
            &format!("/api/v2/conferences/{cid}"),
            Auth::Bearer(&ops.token),
        )
        .await;
    assert!(
        s != StatusCode::SERVICE_UNAVAILABLE,
        "the meeting itself is unaffected"
    );
    let (s, v) = collab::send(
        &app,
        cid,
        g0.auth(),
        json!({ "body": "a private word", "recipientParticipantId": host.participant_id }),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "private chat stays on the meeting store: {v}"
    );
    world.restart_messaging().await;
    let (s, v) = collab::send(&app, cid, g0.auth(), json!({ "body": "back again" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // ---- Rollback: MESSAGING_PRIMARY → SHADOW_VALIDATION copies native messages home.
    let (s, v) = transition(&app, &ops, "SHADOW_VALIDATION", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["pendingRollbackCopies"], 0, "{v}");
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_messages WHERE conference_id = '{cid}' AND origin = 'MESSAGING_ROLLBACK'")).await,
        2
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_messages WHERE conference_id = '{cid}' AND origin = 'MESSAGING_ROLLBACK' AND deleted_at IS NOT NULL")).await,
        1,
        "the moderated native message stays removed"
    );
    let (_, page) = collab::messages(&app, cid, g0.auth(), "").await;
    assert_eq!(page["messages"][0]["body"], "back again", "{page}");
    let (s, v) = collab::send(&app, cid, g0.auth(), json!({ "body": "legacy again" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    um.sync_once().await.unwrap();
    let (_, parity) = app
        .post(
            "/api/v2/admin/chat-migration/parity-runs",
            Auth::Bearer(&ops.token),
            json!({}),
        )
        .await;
    assert_eq!(parity["status"], "PASSED", "{parity}");

    // ---- Retirement is final (application gate and database trigger).
    let (s, v) = transition(&app, &ops, "MESSAGING_PRIMARY", Some("second-operator")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = transition(&app, &ops, "LEGACY_RETIRED", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "retirement needs approval: {v}");
    let (s, v) = transition(&app, &ops, "LEGACY_RETIRED", Some("second-operator")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = transition(&app, &ops, "SHADOW_VALIDATION", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let back = app
        .db
        .conn
        .execute_unprepared("UPDATE conference_chat_migration SET state = 'MESSAGING_PRIMARY', version = version + 1 WHERE id = 1")
        .await;
    assert!(back
        .unwrap_err()
        .to_string()
        .contains("LEGACY_RETIRED is final"));
    let (s, v) = app
        .get("/api/v2/admin/chat-migration", Auth::Bearer(&ops.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["state"], "LEGACY_RETIRED");
    assert_eq!(
        v["transitions"].as_array().unwrap().len(),
        5,
        "LP→SV→MP→SV→MP→LR, all audited"
    );

    // The conference ends → its conversation closes (version-fenced lifecycle sync).
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE conference_sessions SET status = 'ENDED', ended_at = now(), version = version + 1 WHERE id = '{cid}'"
        ))
        .await
        .unwrap();
    let r = um.sync_once().await.unwrap();
    assert_eq!(r.lifecycles_synced, 1, "{r:?}");
    assert_eq!(
        world.m_scalar("SELECT count(*) FROM messaging_conversations WHERE context_class = 'MEETING' AND lifecycle_state = 'CLOSED'").await,
        1
    );
    let (s, v) = collab::send(&app, cid, g0.auth(), json!({ "body": "after the end" })).await;
    assert!(s.is_client_error(), "{v}");
}
