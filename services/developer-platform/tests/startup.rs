//! Developer Platform process startup (the REAL binaries): fail closed on missing or
//! invalid configuration and pending migrations, never echo a secret, serve probes when
//! configured; the worker binary runs its loop against its own database.

mod common;

use std::{path::PathBuf, process::Stdio, time::Duration};

use common::*;
use developer_platform_migration::MigratorTrait;
use serde_json::json;

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("cf-dp-start-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let k = key("developer-platform-test-1");
        std::fs::write(dir.join("key.pem"), k.to_pkcs8_pem().as_bytes()).unwrap();
        let gateway = key("api-gateway-test-1");
        std::fs::write(
            dir.join("trust.json"),
            json!({
                "bundle_version": 1,
                "receiver": cf_service_auth::services::DEVELOPER_PLATFORM,
                "runtime_environment": "test",
                "trusted_issuers": [{
                    "issuer": cf_service_auth::services::API_GATEWAY,
                    "audience": cf_service_auth::services::DEVELOPER_PLATFORM,
                    "runtime_environment": "test",
                    "keys": [gateway.public_jwk()],
                    "permissions": ["developer-request.forward"],
                }],
            })
            .to_string(),
        )
        .unwrap();
        Self { dir }
    }

    fn env(&self, database_url: &str, port: u16) -> Vec<(String, String)> {
        vec![
            ("DATABASE_URL".into(), database_url.into()),
            (
                "DEVELOPER_PLATFORM_BIND_ADDRESS".into(),
                format!("127.0.0.1:{port}"),
            ),
            (
                "DEVELOPER_PLATFORM_DATABASE_MAX_CONNECTIONS".into(),
                "2".into(),
            ),
            ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
            (
                "CF_SERVICE_AUTH_KEY_ID".into(),
                "developer-platform-test-1".into(),
            ),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.dir.join("key.pem").display().to_string(),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.dir.join("trust.json").display().to_string(),
            ),
            (
                "CF_PLATFORM_ACCESS_BASE_URL".into(),
                "http://127.0.0.1:9".into(),
            ),
        ]
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn command(bin: &str, env: &[(String, String)], without: &[&str]) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(bin);
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "developer_platform=warn")
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        if !without.contains(&k.as_str()) {
            c.env(k, v);
        }
    }
    c
}

const SERVER: &str = env!("CARGO_BIN_EXE_developer-platform");
const WORKER: &str = env!("CARGO_BIN_EXE_developer-platform-worker");

#[tokio::test]
async fn missing_or_invalid_configuration_fails_closed_without_echoing_secrets() {
    let files = Files::new();
    let env = files.env("postgres://dp:pw-must-not-leak@127.0.0.1:1/dp", free_port());
    for (missing, extra) in [
        ("DATABASE_URL", None),
        ("CF_PLATFORM_ACCESS_BASE_URL", None),
        ("CF_SERVICE_TRUST_BUNDLE_FILE", None),
        ("", Some(("CF_SERVICE_ID", "cfmeet.platform-access"))),
        (
            "",
            Some(("CF_SERVICE_AUTH_PRIVATE_KEY", "inline-key-material")),
        ),
        (
            "",
            Some(("DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "perhaps")),
        ),
        // HTTPS outside local / test.
        ("", Some(("CF_RUNTIME_ENVIRONMENT", "staging"))),
    ] {
        let mut env = env.clone();
        if let Some((k, v)) = extra {
            env.retain(|(name, _)| name != k);
            env.push((k.into(), v.into()));
        }
        let out = command(SERVER, &env, &[missing]).output().await.unwrap();
        assert!(!out.status.success(), "{missing} {extra:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("pw-must-not-leak"), "{stderr}");
        assert!(!stderr.contains("inline-key-material"), "{stderr}");
    }
    // The database unreachable is an error too (never a half-started server).
    let out = command(SERVER, &env, &[]).output().await.unwrap();
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("pw-must-not-leak"));
}

#[tokio::test]
async fn the_server_refuses_pending_migrations_and_serves_probes_once_migrated() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let files = Files::new();
    let port = free_port();
    let env = files.env(&db.url, port);
    let out = command(SERVER, &env, &[]).output().await.unwrap();
    assert!(!out.status.success(), "pending migrations refuse startup");
    assert!(String::from_utf8_lossy(&out.stderr).contains("pending"));
    developer_platform_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let mut child = command(SERVER, &env, &[]).spawn().unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut ready = None;
    for _ in 0..200 {
        if let Ok(r) = http
            .get(format!("http://127.0.0.1:{port}/ready"))
            .send()
            .await
        {
            ready = Some(r.json::<serde_json::Value>().await.unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let ready = ready.expect("the server answers");
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["gatewayApi"], "disabled");
    let r = http
        .get(format!(
            "http://127.0.0.1:{port}/internal/v1/gateway/context"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        401,
        "unauthenticated internal requests are refused"
    );
    child.kill().await.ok();
}

#[tokio::test]
async fn the_worker_binary_runs_its_own_loop() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let files = Files::new();
    let mut env = files.env(&db.url, free_port());
    env.push(("DEVELOPER_PLATFORM_WORKER_INTERVAL_MS".into(), "100".into()));
    env.push((
        "DEVELOPER_PLATFORM_WORKER_INSTANCE_ID".into(),
        "worker-startup-test".into(),
    ));
    let mut child = command(WORKER, &env, &[]).spawn().unwrap();
    use sea_orm::{ConnectionTrait, Statement};
    let mut beats = 0;
    for _ in 0..100 {
        beats = db
            .conn
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT coalesce(max(beats), 0) FROM devplat_worker_heartbeats WHERE instance_id = 'worker-startup-test'",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<i64>(0)
            .unwrap();
        if beats >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    child.kill().await.ok();
    assert!(beats >= 2, "worker heartbeats: {beats}");
}
