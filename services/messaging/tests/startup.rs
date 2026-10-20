//! Part V C.13: the REAL `messaging` binary runs as a separate process — fail-closed
//! configuration (its own database, service identity, trust bundle, Platform Access
//! target, key FILES only), refusal with pending migrations, a ready server on a migrated
//! database, customer requests failing closed when Platform Access is unreachable, and no
//! secret in its output. No Docker.

mod common;

use std::{net::TcpListener, path::PathBuf, process::Stdio, time::Duration};

use common::TestDb;
use messaging_migration::MigratorTrait;
use uuid::Uuid;

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new(runtime: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("cf-msg-startup-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = common::key("messaging-test-1");
        std::fs::write(dir.join("key.pem"), key.to_pkcs8_pem().as_bytes()).unwrap();
        // A separate Ed25519 key signs key-transparency tree heads (never the service key).
        let kt = common::key("messaging-kt-1");
        std::fs::write(dir.join("kt.pem"), kt.to_pkcs8_pem().as_bytes()).unwrap();
        std::fs::write(
            dir.join("trust.json"),
            serde_json::json!({
                "bundle_version": 1,
                "receiver": cf_service_auth::services::MESSAGING,
                "runtime_environment": runtime,
                "trusted_issuers": [],
            })
            .to_string(),
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).display().to_string()
    }

    fn env(&self, runtime: &str, db_url: &str, bind: &str) -> Vec<(String, String)> {
        vec![
            ("DATABASE_URL".into(), db_url.into()),
            ("CF_RUNTIME_ENVIRONMENT".into(), runtime.into()),
            ("CF_SERVICE_AUTH_KEY_ID".into(), "messaging-test-1".into()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.path("key.pem"),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.path("trust.json"),
            ),
            // Nothing listens there: customer requests must fail closed (503).
            (
                "CF_PLATFORM_ACCESS_BASE_URL".into(),
                "http://127.0.0.1:9".into(),
            ),
            ("MESSAGING_BIND_ADDRESS".into(), bind.into()),
            ("MESSAGING_AUTH_CACHE_TTL_SECONDS".into(), "0".into()),
            (
                "MESSAGING_KT_LOG_ID".into(),
                "cfmeet-messaging:startup".into(),
            ),
            ("MESSAGING_KT_SIGNING_KEY_FILE".into(), self.path("kt.pem")),
        ]
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn command(env: &[(String, String)], cwd: &std::path::Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_messaging"));
    c.env_clear()
        .current_dir(cwd)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "messaging=info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

async fn refuses(env: Vec<(String, String)>, cwd: &std::path::Path, expected: &str) {
    let out = tokio::time::timeout(Duration::from_secs(30), command(&env, cwd).output())
        .await
        .expect("exits")
        .unwrap();
    assert!(!out.status.success(), "started with {env:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(expected),
        "expected `{expected}` in:\n{stderr}"
    );
    for (k, v) in &env {
        if k == "DATABASE_URL" {
            assert!(!stderr.contains(v.as_str()), "the database URL leaked");
        }
    }
}

fn without(env: &[(String, String)], key: &str) -> Vec<(String, String)> {
    env.iter().filter(|(k, _)| k != key).cloned().collect()
}

fn with(env: &[(String, String)], key: &str, value: &str) -> Vec<(String, String)> {
    let mut e = without(env, key);
    e.push((key.into(), value.into()));
    e
}

#[tokio::test]
async fn configuration_fails_closed() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let files = Files::new("test");
    let env = files.env("test", &db.url, "127.0.0.1:0");
    let cwd = files.dir.clone();
    refuses(
        without(&env, "DATABASE_URL"),
        &cwd,
        "DATABASE_URL is not set",
    )
    .await;
    refuses(
        without(&env, "MESSAGING_KT_SIGNING_KEY_FILE"),
        &cwd,
        "MESSAGING_KT_SIGNING_KEY_FILE is not set",
    )
    .await;
    refuses(
        with(
            &env,
            "MESSAGING_KT_SIGNING_KEY_FILE",
            &files.path("trust.json"),
        ),
        &cwd,
        "MESSAGING_KT_SIGNING_KEY_FILE is invalid",
    )
    .await;
    refuses(
        without(&env, "CF_PLATFORM_ACCESS_BASE_URL"),
        &cwd,
        "CF_PLATFORM_ACCESS_BASE_URL is required",
    )
    .await;
    refuses(
        without(&env, "CF_SERVICE_TRUST_BUNDLE_FILE"),
        &cwd,
        "CF_SERVICE_TRUST_BUNDLE_FILE",
    )
    .await;
    refuses(
        with(&env, "CF_SERVICE_ID", "cfmeet.calling"),
        &cwd,
        "CF_SERVICE_ID",
    )
    .await;
    refuses(
        with(&env, "CF_SERVICE_AUTH_PRIVATE_KEY", "inline-key"),
        &cwd,
        "inline private keys are not accepted",
    )
    .await;
    refuses(
        with(&env, "MESSAGING_E2EE_DEVICES_ENABLED", "maybe"),
        &cwd,
        "MESSAGING_E2EE_DEVICES_ENABLED",
    )
    .await;
    // Push adapters: all-or-nothing settings, readable and valid key material.
    refuses(
        with(&env, "MESSAGING_PUSH_APNS_KEY_ID", "KEYID12345"),
        &cwd,
        "MESSAGING_PUSH_APNS_KEY_FILE is invalid",
    )
    .await;
    let apns = [
        ("MESSAGING_PUSH_APNS_KEY_ID", "KEYID12345"),
        ("MESSAGING_PUSH_APNS_TEAM_ID", "TEAMID1234"),
        ("MESSAGING_PUSH_APNS_TOPIC", "com.cfmeet.app"),
    ];
    let mut e = with(
        &env,
        "MESSAGING_PUSH_APNS_KEY_FILE",
        &files.path("trust.json"),
    );
    for (k, v) in apns {
        e = with(&e, k, v);
    }
    refuses(e, &cwd, "the APNs key is not a PKCS#8 P-256 private key").await;
    refuses(
        with(
            &env,
            "MESSAGING_PUSH_WEB_VAPID_KEY_FILE",
            &files.path("trust.json"),
        ),
        &cwd,
        "MESSAGING_PUSH_WEB_VAPID_KEY_FILE is invalid",
    )
    .await;
    refuses(
        with(
            &env,
            "MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE",
            &files.path("missing.json"),
        ),
        &cwd,
        "cannot read the FCM service account file",
    )
    .await;
    // Production refuses plaintext internal targets.
    let prod_files = Files::new("production");
    let prod = prod_files.env("production", &db.url, "127.0.0.1:0");
    refuses(
        prod,
        &prod_files.dir.clone(),
        "service authentication configuration error",
    )
    .await;
}

#[tokio::test]
async fn pending_migrations_are_refused() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let files = Files::new("test");
    let env = files.env("test", &db.url, "127.0.0.1:0");
    refuses(env, &files.dir.clone(), "migration(s) pending").await;
}

#[tokio::test]
async fn a_migrated_server_is_live_ready_and_fails_closed_without_platform_access() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let files = Files::new("test");
    let port = free_port();
    let env = files.env("test", &db.url, &format!("127.0.0.1:{port}"));
    let mut child = command(&env, &files.dir).spawn().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    let mut ready = None;
    for _ in 0..100 {
        if let Ok(r) = client.get(format!("{base}/ready")).send().await {
            ready = Some(r);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ready = ready.expect("the server listens");
    assert_eq!(ready.status(), 200);
    let body: serde_json::Value = ready.json().await.unwrap();
    assert_eq!(body["migrations"], "current");
    assert_eq!(body["e2eeDevices"], "enabled");
    let health = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(health.status(), 200);
    // Platform Access unreachable: customer requests are refused (503), never admitted.
    let r = client
        .post(format!(
            "{base}/v1/messaging/devices/registration-challenges"
        ))
        .header(
            "authorization",
            "Bearer cfmk_test_aaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbb",
        )
        .header("cf-acting-principal", Uuid::new_v4().to_string())
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    // No customer credential, no internal route.
    assert_eq!(
        client
            .get(format!("{base}/internal/v1/x"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    child.start_kill().unwrap();
    let out = child.wait_with_output().await.unwrap();
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(logs.contains("messaging listening"), "{logs}");
    assert!(!logs.contains(&db.url), "database URL in logs");
    assert!(!logs.contains("cfmk_test_aaaa"), "credential in logs");
    assert!(!logs.contains("PRIVATE KEY"), "key material in logs");
}
