//! API Gateway process startup (the REAL binary): fail closed on missing / invalid
//! configuration without echoing secrets; with defaults it is deny-all.

use std::{process::Stdio, time::Duration};

use cf_service_auth::{KeyId, ServiceSigningKey};

const BIN: &str = env!("CARGO_BIN_EXE_api-gateway");

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Dir(std::path::PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup() -> (Dir, Vec<(String, String)>, u16) {
    let dir = std::env::temp_dir().join(format!("cf-gw-start-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = ServiceSigningKey::generate(KeyId::parse("api-gateway-test-1").unwrap());
    std::fs::write(dir.join("key.pem"), key.to_pkcs8_pem().as_bytes()).unwrap();
    let port = free_port();
    let env = vec![
        (
            "API_GATEWAY_BIND_ADDRESS".into(),
            format!("127.0.0.1:{port}"),
        ),
        ("CF_RUNTIME_ENVIRONMENT".into(), "test".into()),
        ("CF_SERVICE_AUTH_KEY_ID".into(), "api-gateway-test-1".into()),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE".into(),
            dir.join("key.pem").display().to_string(),
        ),
        // Nothing listens here: contacting the authority would be a 503.
        (
            "CF_PLATFORM_ACCESS_BASE_URL".into(),
            "http://127.0.0.1:9".into(),
        ),
    ];
    (Dir(dir), env, port)
}

fn command(env: &[(String, String)]) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(BIN);
    c.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "api_gateway=warn")
        .current_dir(std::env::temp_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

#[tokio::test]
async fn invalid_configuration_fails_closed_without_echoing_secrets() {
    let (_dir, env, _) = setup();
    for change in [
        ("CF_PLATFORM_ACCESS_BASE_URL", None),
        ("CF_SERVICE_AUTH_PRIVATE_KEY_FILE", None),
        (
            "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
            Some("/nonexistent/key.pem"),
        ),
        ("DATABASE_URL", Some("postgres://gw:pw-never-echoed@db/gw")),
        ("API_GATEWAY_REDIS_URL", Some("not a url pw-never-echoed")),
        ("CF_SERVICE_AUTH_PRIVATE_KEY", Some("pw-never-echoed")),
        ("API_GATEWAY_PUBLIC_ROUTING_ENABLED", Some("perhaps")),
    ] {
        let mut env = env.clone();
        env.retain(|(k, _)| k != change.0);
        if let Some(v) = change.1 {
            env.push((change.0.into(), v.into()));
        }
        let out = command(&env).output().await.unwrap();
        assert!(!out.status.success(), "{change:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("pw-never-echoed"));
    }
    // Plain-text Redis is refused outside local / test (staging also refuses http targets).
    let mut env2 = env.clone();
    env2.retain(|(k, _)| k != "CF_RUNTIME_ENVIRONMENT" && k != "CF_PLATFORM_ACCESS_BASE_URL");
    env2.push(("CF_RUNTIME_ENVIRONMENT".into(), "staging".into()));
    env2.push((
        "CF_PLATFORM_ACCESS_BASE_URL".into(),
        "https://platform-access.internal".into(),
    ));
    env2.push((
        "API_GATEWAY_REDIS_URL".into(),
        "redis://:pw-never-echoed@cache:6379".into(),
    ));
    let out = command(&env2).output().await.unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("API_GATEWAY_REDIS_URL") && !stderr.contains("pw-never-echoed"),
        "{stderr}"
    );
}

#[tokio::test]
async fn the_default_gateway_is_deny_all() {
    let (_dir, env, port) = setup();
    let mut child = command(&env).spawn().unwrap();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let mut ready = None;
    for _ in 0..200 {
        if let Ok(r) = http.get(format!("{base}/ready")).send().await {
            ready = Some(r.json::<serde_json::Value>().await.unwrap());
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let ready = ready.expect("gateway answers");
    assert_eq!(ready["publicRouting"], "disabled");
    assert_eq!(ready["routes"], 0);
    assert_eq!(ready["websockets"], "disabled");
    for path in [
        "/v1/developer-platform/context",
        "/internal/v1/gateway/context",
        "/provider-callbacks/v1/sms/inbound",
    ] {
        let r = http
            .get(format!("{base}{path}"))
            .bearer_auth("anything")
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.status(),
            404,
            "{path}: default deny, the authority is never contacted"
        );
    }
    child.kill().await.ok();
}
