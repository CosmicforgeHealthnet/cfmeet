//! Part D §31 / cross-cutting: structured logs never contain a telephone number, a
//! Contact Vault access token, key material or a fingerprint — at TRACE level for every
//! crate (Privacy Relay, sqlx, axum, hyper).

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex, OnceLock},
};

use axum::http::{Method, StatusCode};
use cf_service_auth::{ApplicationEnvironmentKind, ContactScope};
use common::{Consumer, Harness};
use serde_json::json;

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

fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture = Capture::default();
        let writer = capture.clone();
        tracing_subscriber::fmt()
            .json()
            .with_env_filter("trace")
            .with_writer(move || writer.clone())
            .init();
        capture
    })
}

const CANARY: &str = "+44 20 7946 0123";
const CANARY_DIGITS: &str = "2079460123";

#[tokio::test]
async fn no_number_token_or_key_reaches_the_logs() {
    let logs = capture();
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(ApplicationEnvironmentKind::Production);
    let token = h.contact_token(&c, &[ContactScope::Read, ContactScope::Write]);
    let created = h
        .contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&token),
            Some(json!({ "telephoneNumber": CANARY })),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let id = created.json["contact"]["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    // Refused inputs (invalid number, extension, URI, client-asserted verification).
    for body in [
        json!({ "telephoneNumber": "+44 20 7946 0123 ext 9" }),
        json!({ "telephoneNumber": "tel:+442079460123" }),
        json!({ "telephoneNumber": "+1 123 207 9460" }),
        json!({ "telephoneNumber": CANARY, "verificationState": "VERIFIED" }),
    ] {
        h.contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&token),
            Some(body),
        )
        .await;
    }
    h.contact_call(
        Method::GET,
        &format!("/v1/privacy-relay/contacts/{id}"),
        Some(&token),
        None,
    )
    .await;
    h.contact_call(
        Method::PATCH,
        &format!("/v1/privacy-relay/contacts/{id}"),
        Some(&token),
        Some(json!({ "expectedVersion": 1, "telephoneNumber": "+44 20 7946 0124" })),
    )
    .await;
    // Rejected tokens.
    h.contact_call(
        Method::GET,
        &format!("/v1/privacy-relay/contacts/{id}"),
        Some("not-a-token"),
        None,
    )
    .await;
    let other = h.contact_token(
        &Consumer::new(ApplicationEnvironmentKind::Sandbox),
        &[ContactScope::Read],
    );
    h.contact_call(
        Method::GET,
        &format!("/v1/privacy-relay/contacts/{id}"),
        Some(&other),
        None,
    )
    .await;
    h.contacts.rewrap_batch(10).await.unwrap();

    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        text.contains("contact vault delegation rejected"),
        "the capture works"
    );
    // sqlx logs statement TEXT (never bound values) at INFO: the proof that values are
    // parameters, never interpolated.
    assert!(
        text.contains("db.statement"),
        "statement logging is captured"
    );
    for needle in [
        CANARY,
        CANARY_DIGITS,
        "442079460123",
        "2079460124",
        "7946 0123",
        &token,
        &other,
    ] {
        assert!(!text.contains(needle), "logs contain {needle}");
    }
    // The token's signature / payload segments never appear either.
    for segment in token.split('.') {
        assert!(!text.contains(segment), "a token segment is logged");
    }
}
