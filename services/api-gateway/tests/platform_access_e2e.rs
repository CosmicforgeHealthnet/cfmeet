//! Cross-service journey with REAL processes: Platform Access (identity authority), the
//! Developer Platform (upstream, own database) and the API Gateway binary.
//!
//! Requires TEST_DATABASE_URL, PLATFORM_ACCESS_BIN / PLATFORM_ACCESS_MIGRATION_BIN and
//! DEVELOPER_PLATFORM_BIN / DEVELOPER_PLATFORM_MIGRATION_BIN (`make api-gateway-integration`
//! builds and sets them). Without them the test FAILS unless CFMEET_SKIP_DB_TESTS=1.
//!
//! Proves: credentials are verified by Platform Access only; the gateway derives the
//! tenant / application / environment from its authority chain and signs it into a
//! short-lived service token; the Developer Platform verifies the token, re-validates
//! the context with Platform Access and anchors canonical ids; forged client headers and
//! payload ids change nothing; revocation and an authority outage fail closed.

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use cf_service_auth::{
    KeyId, OutboundServiceRequest, ServiceId, ServiceSigningKey, ServiceTokenSigner, SystemClock,
    TokenTtl,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const GATEWAY_BIN: &str = env!("CARGO_BIN_EXE_api-gateway");
const PEPPER: &str = "api-gateway-e2e-test-pepper-6f1c2b9e8d7a5c43b21e0f9d";

struct Bins {
    pa: PathBuf,
    pa_migration: PathBuf,
    dp: PathBuf,
    dp_migration: PathBuf,
    admin: String,
}

fn bins() -> Option<Bins> {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    match (
        var("PLATFORM_ACCESS_BIN"),
        var("PLATFORM_ACCESS_MIGRATION_BIN"),
        var("DEVELOPER_PLATFORM_BIN"),
        var("DEVELOPER_PLATFORM_MIGRATION_BIN"),
        std::env::var("TEST_DATABASE_URL").ok().filter(|u| !u.is_empty()),
    ) {
        (Some(pa), Some(pa_migration), Some(dp), Some(dp_migration), Some(admin)) => Some(Bins {
            pa,
            pa_migration,
            dp,
            dp_migration,
            admin,
        }),
        _ if std::env::var("CFMEET_SKIP_DB_TESTS").as_deref() == Ok("1") => {
            eprintln!("SKIPPED: cross-service gateway journey (CFMEET_SKIP_DB_TESTS=1)");
            None
        }
        _ => panic!(
            "TEST_DATABASE_URL, PLATFORM_ACCESS_BIN, PLATFORM_ACCESS_MIGRATION_BIN, DEVELOPER_PLATFORM_BIN \
             and DEVELOPER_PLATFORM_MIGRATION_BIN are required (`make api-gateway-integration`); \
             set CFMEET_SKIP_DB_TESTS=1 to skip explicitly"
        ),
    }
}

fn with_db(admin: &str, db: &str) -> String {
    format!("{}/{db}", admin.rsplit_once('/').unwrap().0)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

/// Databases and files of one journey, removed on drop.
struct Env {
    admin: String,
    dbs: Vec<String>,
    dir: PathBuf,
    children: Vec<tokio::process::Child>,
}

impl Env {
    async fn database(&mut self, prefix: &str) -> String {
        let name = format!(
            "{prefix}_{}_{}",
            std::process::id(),
            Uuid::new_v4().simple()
        );
        let conn = Database::connect(&self.admin).await.unwrap();
        conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        conn.close().await.ok();
        self.dbs.push(name.clone());
        with_db(&self.admin, &name)
    }

    fn write(&self, name: &str, contents: &str) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p.display().to_string()
    }

    async fn spawn(&mut self, bin: &PathBuf, env: Vec<(&str, String)>, ready_url: &str) {
        let mut c = tokio::process::Command::new(bin);
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("RUST_LOG", "error")
            .current_dir(&self.dir)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().expect("process starts");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        for _ in 0..400 {
            if let Ok(Some(status)) = child.try_wait() {
                panic!("{} exited: {status}", bin.display());
            }
            if let Ok(r) = http.get(ready_url).send().await {
                if r.status().is_success() {
                    self.children.push(child);
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{} did not become ready", bin.display());
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.start_kill();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        let admin = self.admin.clone();
        let dbs = std::mem::take(&mut self.dbs);
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                if let Ok(conn) = Database::connect(&admin).await {
                    for db in dbs {
                        let _ = conn
                            .execute_unprepared(&format!(
                                "DROP DATABASE IF EXISTS {db} WITH (FORCE)"
                            ))
                            .await;
                    }
                }
            });
        })
        .join();
    }
}

struct Operator {
    signer: ServiceTokenSigner,
    base: String,
    http: reqwest::Client,
}

impl Operator {
    async fn post(&self, path: &str, body: Value) -> Value {
        let signed = self
            .signer
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::platform_access(),
            ))
            .unwrap();
        let mut r = self
            .http
            .post(format!("{}{path}", self.base))
            .header("x-cf-operator", "api-gateway-e2e")
            .json(&body);
        for (k, v) in signed.headers().iter() {
            r = r.header(k.as_str(), v.as_bytes());
        }
        let r = r.send().await.unwrap();
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        assert!(status.is_success(), "POST {path}: {status} {v}");
        v
    }

    /// tenant -> application (sms.read) -> SANDBOX environment -> service account -> credential.
    async fn customer(&self, classification: &str) -> (String, Value) {
        let slug = format!("t{}", &Uuid::new_v4().simple().to_string()[..12]);
        let t = self
            .post("/internal/v1/admin/tenants", json!({"slug": slug, "displayName": "gateway e2e", "classification": classification}))
            .await;
        let a = self
            .post(
                &format!(
                    "/internal/v1/admin/tenants/{}/applications",
                    t["id"].as_str().unwrap()
                ),
                json!({"slug": "app", "displayName": "gateway e2e app"}),
            )
            .await;
        let app = a["id"].as_str().unwrap();
        self.post(
            &format!("/internal/v1/admin/applications/{app}/scopes"),
            json!({"scope": "sms.read", "privileged": false}),
        )
        .await;
        let e = self
            .post(
                &format!("/internal/v1/admin/applications/{app}/environments"),
                json!({"kind": "SANDBOX"}),
            )
            .await;
        let s = self
            .post(
                &format!(
                    "/internal/v1/admin/environments/{}/service-accounts",
                    e["id"].as_str().unwrap()
                ),
                json!({"name": "backend", "description": "gateway e2e"}),
            )
            .await;
        let c = self
            .post(
                &format!(
                    "/internal/v1/admin/service-accounts/{}/credentials",
                    s["id"].as_str().unwrap()
                ),
                json!({"scopes": ["sms.read"], "label": "gateway-e2e"}),
            )
            .await;
        let chain = json!({
            "tenantId": t["id"], "applicationId": a["id"], "environmentId": e["id"],
            "serviceAccountId": s["id"], "credentialId": c["credential"]["id"],
        });
        (c["secret"].as_str().unwrap().to_string(), chain)
    }
}

async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

#[tokio::test]
async fn customers_reach_the_developer_platform_only_with_platform_access_authority() {
    let Some(b) = bins() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("cf-gw-e2e-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut env = Env {
        admin: b.admin.clone(),
        dbs: vec![],
        dir,
        children: vec![],
    };

    let (pa_key, gw_key, dp_key, op_key) = (
        key("platform-access-e2e-1"),
        key("api-gateway-e2e-1"),
        key("developer-platform-e2e-1"),
        key("operator-e2e-1"),
    );
    let issuer = |id: &str, k: &ServiceSigningKey, perms: &[&str], audience: &str| {
        json!({"issuer": id, "audience": audience, "runtime_environment": "test",
               "keys": [k.public_jwk()], "permissions": perms})
    };
    use cf_service_auth::services as s;
    // Platform Access: the gateway authenticates credentials, the Developer Platform reads
    // identity; nothing else.
    let pa_bundle = json!({"bundle_version": 1, "receiver": s::PLATFORM_ACCESS, "runtime_environment": "test",
    "trusted_issuers": [
        issuer(s::API_GATEWAY, &gw_key, &["platform-credential.authenticate"], s::PLATFORM_ACCESS),
        issuer(s::DEVELOPER_PLATFORM, &dp_key, &["platform-identity.read"], s::PLATFORM_ACCESS),
        issuer(s::OPERATOR_CONSOLE, &op_key, &["platform-identity.administer"], s::PLATFORM_ACCESS),
    ]});
    let dp_bundle = json!({"bundle_version": 1, "receiver": s::DEVELOPER_PLATFORM, "runtime_environment": "test",
        "trusted_issuers": [issuer(s::API_GATEWAY, &gw_key, &["developer-request.forward"], s::DEVELOPER_PLATFORM)]});

    // Platform Access.
    let pa_db = env.database("gwe2e_pa").await;
    let out = std::process::Command::new(&b.pa_migration)
        .arg("up")
        .env("DATABASE_URL", &pa_db)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pa_port = free_port();
    let pa_url = format!("http://127.0.0.1:{pa_port}");
    let pa_env = vec![
        ("DATABASE_URL", pa_db.clone()),
        ("PLATFORM_ACCESS_DATABASE_MAX_CONNECTIONS", "3".into()),
        (
            "PLATFORM_ACCESS_BIND_ADDRESS",
            format!("127.0.0.1:{pa_port}"),
        ),
        ("API_CREDENTIAL_PEPPER", PEPPER.into()),
        ("CF_RUNTIME_ENVIRONMENT", "test".into()),
        ("CF_SERVICE_AUTH_KEY_ID", "platform-access-e2e-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            env.write("pa.pem", &pa_key.to_pkcs8_pem()),
        ),
        (
            "CF_SERVICE_TRUST_BUNDLE_FILE",
            env.write("pa-trust.json", &pa_bundle.to_string()),
        ),
    ];
    env.spawn(&b.pa, pa_env, &format!("{pa_url}/ready")).await;

    // Developer Platform (own database, migrated by its own migration binary).
    let dp_db_url = env.database("gwe2e_dp").await;
    let out = std::process::Command::new(&b.dp_migration)
        .arg("up")
        .env("DATABASE_URL", &dp_db_url)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dp_port = free_port();
    let dp_url = format!("http://127.0.0.1:{dp_port}");
    let dp_env = vec![
        ("DATABASE_URL", dp_db_url.clone()),
        (
            "DEVELOPER_PLATFORM_BIND_ADDRESS",
            format!("127.0.0.1:{dp_port}"),
        ),
        ("DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS", "2".into()),
        ("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true".into()),
        ("CF_RUNTIME_ENVIRONMENT", "test".into()),
        ("CF_SERVICE_AUTH_KEY_ID", "developer-platform-e2e-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            env.write("dp.pem", &dp_key.to_pkcs8_pem()),
        ),
        (
            "CF_SERVICE_TRUST_BUNDLE_FILE",
            env.write("dp-trust.json", &dp_bundle.to_string()),
        ),
        ("CF_PLATFORM_ACCESS_BASE_URL", pa_url.clone()),
    ];
    env.spawn(&b.dp, dp_env, &format!("{dp_url}/ready")).await;

    // The API Gateway binary, public routing enabled.
    let gw_port = free_port();
    let gw = format!("http://127.0.0.1:{gw_port}");
    let gw_env = vec![
        ("API_GATEWAY_BIND_ADDRESS", format!("127.0.0.1:{gw_port}")),
        ("API_GATEWAY_PUBLIC_ROUTING_ENABLED", "true".into()),
        ("CF_RUNTIME_ENVIRONMENT", "test".into()),
        ("CF_SERVICE_AUTH_KEY_ID", "api-gateway-e2e-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            env.write("gw.pem", &gw_key.to_pkcs8_pem()),
        ),
        ("CF_PLATFORM_ACCESS_BASE_URL", pa_url.clone()),
        ("CF_DEVELOPER_PLATFORM_BASE_URL", dp_url.clone()),
    ];
    env.spawn(&PathBuf::from(GATEWAY_BIN), gw_env, &format!("{gw}/ready"))
        .await;

    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let operator = Operator {
        signer: ServiceTokenSigner::new(
            ServiceId::operator_console(),
            cf_service_auth::RuntimeEnvironment::Test,
            op_key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ),
        base: pa_url.clone(),
        http: http.clone(),
    };
    let (secret, chain) = operator.customer("EXTERNAL").await;
    let (other_secret, other_chain) = operator.customer("FIRST_PARTY").await;
    let path = format!("{gw}/v1/developer-platform/context");

    // 1. The credential's own chain, confirmed by Platform Access, anchored by the Developer
    //    Platform — whatever the client claims in headers or query.
    let r = http
        .get(format!("{path}?tenant_id=evil"))
        .bearer_auth(&secret)
        .header("x-tenant-id", other_chain["tenantId"].as_str().unwrap())
        .header("x-cf-delegated-context", "{}")
        .header(
            "x-cf-correlation-id",
            "corr_00000000000000000000000000000000",
        )
        .send()
        .await
        .unwrap();
    let correlation = r
        .headers()
        .get("x-correlation-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let status = r.status();
    let body: Value = r.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    for k in [
        "tenantId",
        "applicationId",
        "environmentId",
        "serviceAccountId",
    ] {
        assert_eq!(body[k], chain[k], "{k}");
    }
    assert_eq!(body["authority"], "PLATFORM_ACCESS");
    assert_eq!(body["forwardedBy"], "cfmeet.api-gateway");
    assert_eq!(
        body["correlationId"], correlation,
        "one correlation id across every hop"
    );
    // 2. First-party and external tenants use the same route; each sees only itself.
    let body: Value = http
        .get(&path)
        .bearer_auth(&other_secret)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["tenantId"], other_chain["tenantId"]);
    let dp = Database::connect(&dp_db_url).await.unwrap();
    assert_eq!(
        count(&dp, "SELECT count(*) FROM devplat_environment_refs").await,
        2
    );
    // 3. Unknown credentials, a client service token and unknown routes are refused.
    assert_eq!(
        http.get(&path)
            .bearer_auth("cfk_test_not_a_real_credential")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(http.get(&path).send().await.unwrap().status(), 401);
    let forged = ServiceTokenSigner::new(
        ServiceId::api_gateway(),
        cf_service_auth::RuntimeEnvironment::Test,
        key("api-gateway-e2e-1"),
        TokenTtl::default(),
        Arc::new(SystemClock),
    )
    .sign(&OutboundServiceRequest::new_operation(
        ServiceId::developer_platform(),
    ))
    .unwrap();
    let mut direct = http.get(format!("{dp_url}/internal/v1/gateway/context"));
    for (k, v) in forged.headers().iter() {
        direct = direct.header(k.as_str(), v.as_bytes());
    }
    assert_eq!(
        direct.send().await.unwrap().status(),
        401,
        "a forged gateway token is rejected upstream"
    );
    for p in [
        "/internal/v1/gateway/context",
        "/internal/v1/credentials/authenticate",
        "/v1/admin/tenants",
    ] {
        assert_eq!(
            http.get(format!("{gw}{p}"))
                .bearer_auth(&secret)
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{p}"
        );
    }
    // 4. Revocation at the authority takes effect at the ingress.
    operator
        .post(
            &format!(
                "/internal/v1/admin/credentials/{}/revoke",
                chain["credentialId"].as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(
        http.get(&path)
            .bearer_auth(&secret)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    // 5. The authority unreachable fails closed.
    let pa = env.children.remove(0);
    let mut pa = pa;
    pa.kill().await.ok();
    let r = http
        .get(&path)
        .bearer_auth(&other_secret)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    dp.close().await.ok();
}
