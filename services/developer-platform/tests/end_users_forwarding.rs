//! ADR 0062 Partner Users: the Developer Platform's HTTP adapter names the gateway-forwarded
//! credential to Platform Access on every Users API call. Platform Access refuses a call
//! without `CF-Forwarded-Credential-Id` (DELEGATED_CONTEXT_REQUIRED); the first production
//! activation failed exactly that way because the shared client did not forward it.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::Duration,
};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    ApplicationEnvironmentKind, CorrelationId, DelegatedPlatformContext, KeyId, RuntimeEnvironment,
    ServiceId, ServiceSigningKey, ServiceTokenSigner, SystemClock, TokenTtl,
};
use developer_platform::platform_access::{
    EndUserCall, EndUsersReply, ForwardedCredential, HttpPlatformAccess, PlatformAccessPort,
};
use serde_json::json;
use uuid::Uuid;

/// A one-shot stand-in for Platform Access: answers 201 `{"id": ...}` and returns the raw
/// request it read (headers only).
fn platform_access() -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = s.read(&mut chunk).unwrap_or(0);
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let body = br#"{"id":"7d1f0c2a-5b3e-4c8d-9e6f-0a1b2c3d4e5f"}"#;
        let head = format!(
            "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).unwrap();
        s.write_all(body).unwrap();
        String::from_utf8_lossy(&buf).to_string()
    });
    (url, handle)
}

#[tokio::test]
async fn every_partner_users_call_names_the_forwarded_credential() {
    let (url, server) = platform_access();
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::developer_platform(),
        RuntimeEnvironment::Local,
        ServiceSigningKey::generate(KeyId::parse("developer-platform-test-1").unwrap()),
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(RuntimeEnvironment::Local);
    targets.insert(ServiceId::platform_access(), &url).unwrap();
    let client = InternalServiceClient::new(signer, targets, Duration::from_secs(5)).unwrap();
    let adapter = HttpPlatformAccess::new(Arc::new(client));

    let credential = Uuid::new_v4();
    let forwarded = ForwardedCredential {
        delegated: DelegatedPlatformContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            ApplicationEnvironmentKind::Sandbox,
            Uuid::new_v4(),
        )
        .with_initiating_credential(credential),
        credential_id: credential,
    };
    let reply = adapter
        .end_users(
            EndUserCall::Create {
                body: json!({ "externalId": "rider-1", "displayName": "Rider One" }),
            },
            &forwarded,
            &CorrelationId::generate(),
        )
        .await
        .expect("the adapter reaches Platform Access");
    assert!(
        matches!(reply, EndUsersReply::Ok { status: 201, .. }),
        "{reply:?}"
    );

    let raw = server.join().unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("post /internal/v1/end-users "), "{raw}");
    assert!(
        raw.contains(&format!("cf-forwarded-credential-id: {credential}")),
        "the forwarded credential id must reach Platform Access: {raw}"
    );
    assert!(raw.contains("cf-service-authorization: bearer "), "{raw}");
}
