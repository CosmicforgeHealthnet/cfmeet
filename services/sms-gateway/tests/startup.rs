//! Part IV M: the real `sms-gateway` binary — fail-closed configuration (its own database,
//! service identity, trust bundle, Platform Access target, key FILES only), refusal of
//! the deterministic provider outside local / test, refusal to serve with pending
//! migrations, and a ready server on a migrated database. No Docker.
mod common;

use std::{path::PathBuf, process::Stdio, time::Duration};

use base64::Engine;
use common::TestDb;
use sms_gateway_migration::MigratorTrait;
use uuid::Uuid;

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new(runtime: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("cf-sg-startup-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = common::key("sms-gateway-test-1");
        std::fs::write(dir.join("key.pem"), key.to_pkcs8_pem().as_bytes()).unwrap();
        std::fs::write(
            dir.join("trust.json"),
            serde_json::json!({
                "bundle_version": 1,
                "receiver": cf_service_auth::services::SMS_GATEWAY,
                "runtime_environment": runtime,
                "trusted_issuers": [],
            })
            .to_string(),
        )
        .unwrap();
        let b64 = |b: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(b);
        std::fs::write(dir.join("content.key"), b64(common::key_bytes(7))).unwrap();
        std::fs::write(dir.join("fingerprint.key"), b64(common::key_bytes(11))).unwrap();
        std::fs::write(
            dir.join("callback.secret"),
            b64(common::CALLBACK_SECRET.to_vec()),
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).display().to_string()
    }

    fn env(&self, runtime: &str, db_url: &str) -> Vec<(String, String)> {
        let platform = if runtime == "test" || runtime == "local" {
            "http://127.0.0.1:9"
        } else {
            "https://platform-access.cfmeet.invalid"
        };
        vec![
            ("DATABASE_URL".into(), db_url.into()),
            ("CF_RUNTIME_ENVIRONMENT".into(), runtime.into()),
            ("CF_SERVICE_AUTH_KEY_ID".into(), "sms-gateway-test-1".into()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.path("key.pem"),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.path("trust.json"),
            ),
            // Never contacted at startup; customer requests would fail closed (503).
            ("CF_PLATFORM_ACCESS_BASE_URL".into(), platform.into()),
            ("SMS_GATEWAY_CONTENT_KEY_ID".into(), "content-1".into()),
            (
                "SMS_GATEWAY_CONTENT_KEY_FILE".into(),
                self.path("content.key"),
            ),
            (
                "SMS_GATEWAY_FINGERPRINT_KEY_FILE".into(),
                self.path("fingerprint.key"),
            ),
        ]
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn command(env: &[(String, String)], cwd: &std::path::Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_sms-gateway"));
    c.env_clear()
        .current_dir(cwd) // no stray .env file is picked up
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "sms_gateway=warn")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

async fn refuses(env: Vec<(String, String)>, cwd: &std::path::Path, expected_stderr: &str) {
    let out = tokio::time::timeout(Duration::from_secs(30), command(&env, cwd).output())
        .await
        .expect("exits")
        .unwrap();
    assert!(!out.status.success(), "started with {env:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(expected_stderr),
        "expected `{expected_stderr}` in:\n{stderr}"
    );
    for (k, v) in &env {
        if k == "DATABASE_URL" {
            assert!(!stderr.contains(v.as_str()), "the database URL leaked");
        }
    }
    let secret = String::from_utf8_lossy(common::CALLBACK_SECRET).to_string();
    assert!(!stderr.contains(&secret), "a secret leaked");
}

fn with(
    mut env: Vec<(String, String)>,
    extra: &[(&str, &str)],
    without: &[&str],
) -> Vec<(String, String)> {
    env.retain(|(k, _)| !without.contains(&k.as_str()));
    env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    env
}

#[tokio::test]
async fn the_binary_fails_closed_on_incomplete_or_unsafe_configuration() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let files = Files::new("test");
    let base = files.env("test", &db.url);
    let dir = &files.dir;
    refuses(vec![], dir, "DATABASE_URL").await;
    for missing in [
        "CF_SERVICE_AUTH_KEY_ID",
        "CF_SERVICE_TRUST_BUNDLE_FILE",
        "SMS_GATEWAY_CONTENT_KEY_FILE",
        "SMS_GATEWAY_FINGERPRINT_KEY_FILE",
    ] {
        refuses(with(base.clone(), &[], &[missing]), dir, missing).await;
    }
    refuses(
        with(base.clone(), &[], &["CF_PLATFORM_ACCESS_BASE_URL"]),
        dir,
        "CF_PLATFORM_ACCESS_BASE_URL is required",
    )
    .await;
    // Key material is accepted from files only.
    refuses(
        with(base.clone(), &[("SMS_GATEWAY_CONTENT_KEY", "inline")], &[]),
        dir,
        "inline key material is not accepted",
    )
    .await;
    refuses(
        with(
            base.clone(),
            &[("SMS_GATEWAY_PROVIDER_CALLBACK_SECRET", "inline")],
            &[],
        ),
        dir,
        "inline key material is not accepted",
    )
    .await;
    refuses(
        with(base.clone(), &[("CF_SERVICE_ID", "cfmeet.numbering")], &[]),
        dir,
        "CF_SERVICE_ID",
    )
    .await;
    refuses(
        with(base.clone(), &[("SMS_GATEWAY_PROVIDER", "carrier-x")], &[]),
        dir,
        "SMS_GATEWAY_PROVIDER",
    )
    .await;
    refuses(
        with(
            base.clone(),
            &[("SMS_GATEWAY_PROVIDER", "deterministic")],
            &[],
        ),
        dir,
        "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
    )
    .await;
    // The same key for content and fingerprints is refused.
    let same = files.path("content.key");
    refuses(
        with(
            base.clone(),
            &[("SMS_GATEWAY_FINGERPRINT_KEY_FILE", same.as_str())],
            &["SMS_GATEWAY_FINGERPRINT_KEY_FILE"],
        ),
        dir,
        "must differ from the content key",
    )
    .await;
}

#[tokio::test]
async fn the_deterministic_provider_is_refused_in_staging_and_production() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    for runtime in ["staging", "production"] {
        let files = Files::new(runtime);
        let secret = files.path("callback.secret");
        let env = with(
            files.env(runtime, &db.url),
            &[
                ("SMS_GATEWAY_PROVIDER", "deterministic"),
                ("SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE", secret.as_str()),
            ],
            &[],
        );
        refuses(env, &files.dir, "test-only SMS provider is refused").await;
    }
}

#[tokio::test]
async fn the_binary_refuses_pending_migrations_and_serves_once_migrated() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let files = Files::new("test");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let secret = files.path("callback.secret");
    let bind = format!("127.0.0.1:{port}");
    let env = with(
        files.env("test", &db.url),
        &[
            ("SMS_GATEWAY_BIND_ADDRESS", bind.as_str()),
            ("SMS_GATEWAY_DATABASE_MAX_CONNECTIONS", "3"),
            ("SMS_GATEWAY_PROVIDER", "deterministic"),
            ("SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE", secret.as_str()),
        ],
        &[],
    );
    refuses(env.clone(), &files.dir, "migration(s) pending").await;
    sms_gateway_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let mut child = command(&env, &files.dir).spawn().unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut ready = None;
    for _ in 0..400 {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("sms-gateway exited: {status}");
        }
        if let Ok(r) = http.get(format!("http://{bind}/ready")).send().await {
            ready = Some((
                r.status().as_u16(),
                r.json::<serde_json::Value>().await.unwrap(),
            ));
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, body) = ready.expect("the server answers");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["provider"], "configured");
    // The customer API fails closed while Platform Access is unreachable.
    let r = http
        .get(format!("http://{bind}/v1/sms/anything"))
        .bearer_auth("cfk_test_x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 503);
    let served: serde_json::Value = http
        .get(format!("http://{bind}/openapi.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(served["info"]["title"], "CF Meet SMS Gateway API");
    child.kill().await.ok();
}
