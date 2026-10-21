//! A first-party backend may forward an end user's `Idempotency-Key` on an internal call,
//! and nothing else: authorization / identity headers cannot be forwarded or overridden.
#![cfg(feature = "client")]

mod common;

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::Duration,
};

use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient, InternalTargets},
    OutboundServiceRequest, RuntimeEnvironment, ServiceId,
};
use common::*;
use http::Method;

/// A one-shot HTTP server that answers 200 `{}` and returns the raw request it read.
fn one_shot() -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).unwrap_or(0);
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
            .unwrap();
        String::from_utf8_lossy(&buf).to_string()
    });
    (url, handle)
}

fn client(url: &str) -> InternalServiceClient {
    let clock = clock();
    let me = ServiceId::meeting_service();
    let signer = Arc::new(signer(
        &me,
        RuntimeEnvironment::Local,
        key("meeting-1"),
        &clock,
    ));
    let mut targets = InternalTargets::new(RuntimeEnvironment::Local);
    targets.insert(ServiceId::messaging(), url).unwrap();
    InternalServiceClient::new(signer, targets, Duration::from_secs(5)).unwrap()
}

#[tokio::test]
async fn the_idempotency_key_is_forwarded_alongside_the_service_token() {
    let (url, server) = one_shot();
    let c = client(&url);
    let r = c
        .exchange_json_query_with_headers::<serde_json::Value>(
            &OutboundServiceRequest::new_operation(ServiceId::messaging()),
            Method::POST,
            "/internal/v1/delegated/v1/conversations",
            &[],
            &[("Idempotency-Key", "web-7c1f2d9e")],
            Some(&serde_json::json!({})),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let raw = server.join().unwrap().to_ascii_lowercase();
    assert!(raw.contains("idempotency-key: web-7c1f2d9e"), "{raw}");
    assert!(raw.contains("cf-service-authorization: bearer "), "{raw}");
}

#[tokio::test]
async fn other_headers_and_unsafe_values_are_refused_before_sending() {
    let c = client("http://127.0.0.1:9");
    for (name, value) in [
        ("Authorization", "Bearer x"),
        ("CF-Service-Authorization", "Bearer forged"),
        (
            "CF-Acting-Principal",
            "00000000-0000-0000-0000-000000000000",
        ),
        ("X-Forwarded-For", "10.0.0.1"),
        ("Idempotency-Key", ""),
        ("Idempotency-Key", "has space"),
        ("Idempotency-Key", "line\r\nbreak"),
    ] {
        let long = "k".repeat(129);
        for v in [value, long.as_str()] {
            if name != "Idempotency-Key" && v == long {
                continue;
            }
            let e = c
                .exchange_json_query_with_headers::<serde_json::Value>(
                    &OutboundServiceRequest::new_operation(ServiceId::messaging()),
                    Method::POST,
                    "/internal/v1/delegated/v1/conversations",
                    &[],
                    &[(name, v)],
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(e, InternalCallError::InvalidHeader, "{name}: {v:?}");
        }
    }
}
