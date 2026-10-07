//! I. Secret leakage: raw credentials never appear in error bodies, Debug output or
//! structured logs.

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::{Method, StatusCode};
use calling::{
    config::CallingConfig,
    domain::{EnvironmentKind, TenantClassification},
    platform::{credentials::RotateCredential, scopes::names},
};
use common::{secret_part, Harness, TEST_PEPPER};
use serde_json::json;
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn structured_logs_never_contain_credentials_or_authorization_headers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(capture.clone()),
    );
    let _guard = tracing::subscriber::set_default(subscriber);

    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let token = issued.secret.expose_once().to_string();
    let wrong = format!("{}0000", &token[..token.len() - 4]);

    assert_eq!(h.whoami(&token).await.status, StatusCode::OK);
    assert_eq!(h.whoami(&wrong).await.status, StatusCode::UNAUTHORIZED);
    let scope_denied = h
        .request(Method::GET, "/test/calls-write", Some(&token), None)
        .await;
    assert_eq!(scope_denied.status, StatusCode::FORBIDDEN);
    let rotated = h
        .platform
        .rotate_credential(issued.credential.id, RotateCredential::default())
        .await
        .unwrap();
    let rotated_token = rotated.secret.expose_once().to_string();
    h.platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    assert_eq!(h.whoami(&token).await.status, StatusCode::UNAUTHORIZED);

    let logs = capture.text();
    // Logging actually happened, with operator context.
    assert!(logs.contains("API credential issued"), "{logs}");
    assert!(logs.contains("platform authentication rejected"));
    assert!(logs.contains("secret_mismatch"));
    assert!(logs.contains(&issued.credential.public_id));
    assert!(logs.contains("req_"));
    // After authentication the request span carries the platform identifiers.
    let scope_line = logs
        .lines()
        .find(|l| l.contains("scope check denied"))
        .expect("scope denial logged");
    for id in [
        c.tenant.id.to_string(),
        c.application.id.to_string(),
        c.env(EnvironmentKind::Production).id.to_string(),
        c.sa(EnvironmentKind::Production).to_string(),
        issued.credential.id.to_string(),
    ] {
        assert!(scope_line.contains(&id), "{scope_line}");
    }
    assert!(scope_line.contains("\"request_id\":\"req_"), "{scope_line}");

    for secret in [&token, &wrong, &rotated_token] {
        assert!(!logs.contains(secret.as_str()), "raw credential in logs");
        assert!(!logs.contains(secret_part(secret)), "secret part in logs");
    }
    assert!(!logs.to_lowercase().contains("bearer "));
    assert!(!logs.to_lowercase().contains("authorization"));
    assert!(!logs.contains(TEST_PEPPER));
}

#[tokio::test]
async fn error_bodies_never_echo_presented_credentials() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once().to_string();
    let wrong = format!("{}ffff", &token[..token.len() - 4]);
    for presented in [&token, &wrong] {
        for (method, path) in [
            (Method::GET, "/test/calls-write"),
            (Method::POST, "/test/resolve"),
            (Method::POST, "/test/context"),
        ] {
            let response = h
                .request(
                    method,
                    path,
                    Some(presented),
                    Some(json!({ "credential": presented })),
                )
                .await;
            assert!(
                !response.text.contains(secret_part(presented)),
                "{path}: {}",
                response.text
            );
            for value in response.headers.values() {
                assert!(!value
                    .to_str()
                    .unwrap_or("")
                    .contains(secret_part(presented)));
            }
        }
    }
    h.platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    let revoked = h.whoami(&token).await;
    assert!(!revoked.text.contains(secret_part(&token)));
    assert!(!revoked.text.to_lowercase().contains("revoked"));
}

#[tokio::test]
async fn debug_output_of_secret_bearing_types_is_redacted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once().to_string();
    for rendered in [
        format!("{issued:?}"),
        format!("{issued:#?}"),
        format!("{:?}", issued.secret),
    ] {
        assert!(!rendered.contains(secret_part(&token)), "{rendered}");
        assert!(rendered.contains("[REDACTED]"));
    }
    // Authentication errors and contexts carry no secret either.
    let wrong = format!("{}0000", &token[..token.len() - 4]);
    let err = h.authenticate(&wrong).await.unwrap_err();
    assert!(!format!("{err:?}").contains(secret_part(&wrong)));
    let ctx = h.ctx(&token).await;
    assert!(!format!("{ctx:?}").contains(secret_part(&token)));

    // Configuration Debug never shows the pepper or database password.
    let config = CallingConfig::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://calling:db-password-123@db/calling".into()),
        "API_CREDENTIAL_PEPPER" => Some(TEST_PEPPER.into()),
        _ => None,
    })
    .unwrap();
    let rendered = format!("{config:?}");
    assert!(!rendered.contains(TEST_PEPPER));
    assert!(!rendered.contains("db-password-123"));
}
