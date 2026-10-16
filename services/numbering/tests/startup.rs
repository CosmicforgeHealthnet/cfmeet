//! Part IV C1 / C6: the real `numbering` binary — fail-closed configuration (database,
//! service identity, trust bundle, Platform Access target, no inline keys), refusal to
//! serve with pending migrations, and a ready server on a migrated database. No Docker.
mod common;

use std::{path::PathBuf, process::Stdio, time::Duration};

use cf_service_auth::{KeyId, ServiceSigningKey};
use common::TestDb;
use numbering_migration::MigratorTrait;
use uuid::Uuid;

struct Files {
    dir: PathBuf,
}

impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("cf-nb-startup-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = ServiceSigningKey::generate(KeyId::parse("numbering-test-1").unwrap());
        std::fs::write(dir.join("key.pem"), key.to_pkcs8_pem().as_bytes()).unwrap();
        std::fs::write(
            dir.join("trust.json"),
            common::trust_bundle_json(cf_service_auth::services::NUMBERING, &[]),
        )
        .unwrap();
        Self { dir }
    }

    fn auth_env(&self) -> Vec<(String, String)> {
        vec![
            ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
            ("CF_SERVICE_AUTH_KEY_ID".into(), "numbering-test-1".into()),
            (
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
                self.dir.join("key.pem").display().to_string(),
            ),
            (
                "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
                self.dir.join("trust.json").display().to_string(),
            ),
            // Never contacted at startup; customer requests would fail closed (503).
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

fn command(env: &[(String, String)], cwd: &std::path::Path) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_numbering"));
    c.env_clear()
        .current_dir(cwd) // no stray .env file is picked up
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "numbering=warn")
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
    // Secrets never appear in a refusal.
    for (k, v) in &env {
        if k == "DATABASE_URL" {
            assert!(!stderr.contains(v.as_str()), "the database URL leaked");
        }
    }
}

#[tokio::test]
async fn the_binary_fails_closed_on_incomplete_or_unsafe_configuration() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let files = Files::new();
    let db_env = [("DATABASE_URL".to_string(), db.url.clone())];
    let with = |extra: Vec<(&str, &str)>, without: &[&str]| {
        let mut env: Vec<(String, String)> = db_env
            .iter()
            .cloned()
            .chain(files.auth_env())
            .filter(|(k, _)| !without.contains(&k.as_str()))
            .collect();
        env.extend(
            extra
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        env
    };
    refuses(vec![], &files.dir, "DATABASE_URL").await;
    refuses(
        with(vec![], &["CF_SERVICE_AUTH_KEY_ID"]),
        &files.dir,
        "CF_SERVICE_AUTH_KEY_ID",
    )
    .await;
    refuses(
        with(vec![], &["CF_SERVICE_TRUST_BUNDLE_FILE"]),
        &files.dir,
        "CF_SERVICE_TRUST_BUNDLE_FILE",
    )
    .await;
    refuses(
        with(vec![], &["CF_PLATFORM_ACCESS_BASE_URL"]),
        &files.dir,
        "CF_PLATFORM_ACCESS_BASE_URL is required",
    )
    .await;
    refuses(
        with(vec![("CF_SERVICE_AUTH_PRIVATE_KEY", "inline")], &[]),
        &files.dir,
        "inline private keys are not accepted",
    )
    .await;
    refuses(
        with(vec![("CF_SERVICE_ID", "cfmeet.calling")], &[]),
        &files.dir,
        "CF_SERVICE_ID",
    )
    .await;
    refuses(
        with(vec![("NUMBERING_WORKER_LEASE_SECONDS", "0")], &[]),
        &files.dir,
        "NUMBERING_WORKER_LEASE_SECONDS",
    )
    .await;
    // A trust bundle for another receiver is refused.
    std::fs::write(
        files.dir.join("other.json"),
        common::trust_bundle_json(cf_service_auth::services::CALLING, &[]),
    )
    .unwrap();
    let mut env = with(vec![], &["CF_SERVICE_TRUST_BUNDLE_FILE"]);
    env.push((
        "CF_SERVICE_TRUST_BUNDLE_FILE".into(),
        files.dir.join("other.json").display().to_string(),
    ));
    let out = command(&env, &files.dir).output().await.unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[tokio::test]
async fn the_binary_refuses_pending_migrations_and_serves_once_migrated() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let files = Files::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut env: Vec<(String, String)> = vec![
        ("DATABASE_URL".into(), db.url.clone()),
        ("NUMBERING_BIND_ADDRESS".into(), format!("127.0.0.1:{port}")),
        ("NUMBERING_DATABASE_MAX_CONNECTIONS".into(), "3".into()),
    ];
    env.extend(files.auth_env());
    refuses(env.clone(), &files.dir, "migration(s) pending").await;
    numbering_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    let mut child = command(&env, &files.dir).spawn().unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut ready = None;
    for _ in 0..400 {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("numbering exited: {status}");
        }
        if let Ok(r) = http
            .get(format!("http://127.0.0.1:{port}/ready"))
            .send()
            .await
        {
            ready = Some((
                r.status().as_u16(),
                r.json::<serde_json::Value>().await.unwrap(),
            ));
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (status, body) = ready.expect("numbering became reachable");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["migrations"], "current");
    let health = http
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    // Customer routes fail closed without a reachable Platform Access; internal routes
    // refuse unauthenticated callers.
    let customer = http
        .get(format!("http://127.0.0.1:{port}/v1/numbers/assignments"))
        .bearer_auth("cfk_test_anything")
        .send()
        .await
        .unwrap();
    assert_eq!(customer.status(), 503);
    let internal = http
        .get(format!(
            "http://127.0.0.1:{port}/internal/v1/number-resources/{}",
            Uuid::new_v4()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(internal.status(), 401);
    child.kill().await.unwrap();
}
