//! Messaging's Platform Access adapters read Platform Access's own refusals (401 / 403 /
//! 404) as answers, not as an outage. The shared client's `get` / `post_json` turn every
//! non-2xx status into an error, so an adapter that matched on those statuses after them
//! never saw them: the first production `activate-partner-chat` answered a revoked user
//! token with 503 SERVICE_UNAVAILABLE instead of 401 (ADR 0062).

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    time::Duration,
};

use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, SystemClock,
    TokenTtl,
};
use messaging::{
    identity::{
        CustomerAuthority, DelegatedScope, EnvironmentKind, OwnerScope, PlatformAccessAuthority,
    },
    partner::{PartnerDirectory, PlatformAccessPartners, Verification},
};
use uuid::Uuid;

/// A one-shot stand-in for Platform Access answering `status` with an error envelope
/// carrying `code`.
fn platform_access(status: u16, code: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let body = format!(r#"{{"error":{{"code":"{code}","message":"refused"}}}}"#);
    std::thread::spawn(move || {
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
        let head = format!(
            "HTTP/1.1 {status} Refused\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).unwrap();
        s.write_all(body.as_bytes()).unwrap();
    });
    url
}

fn client(url: &str) -> Arc<InternalServiceClient> {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        RuntimeEnvironment::Local,
        ServiceSigningKey::generate(KeyId::parse("messaging-test-1").unwrap()),
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(RuntimeEnvironment::Local);
    targets.insert(ServiceId::platform_access(), url).unwrap();
    Arc::new(InternalServiceClient::new(signer, targets, Duration::from_secs(5)).unwrap())
}

fn scope() -> OwnerScope {
    OwnerScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: Uuid::new_v4(),
    }
}

#[tokio::test]
async fn a_revoked_user_token_is_a_rejection_not_an_outage() {
    for code in [
        "USER_TOKEN_REVOKED",
        "USER_TOKEN_EXPIRED",
        "USER_TOKEN_INVALID",
    ] {
        let partners = PlatformAccessPartners::new(client(&platform_access(401, code)));
        let answer = partners
            .verify_user_token("cfut_sbx_x.y.z")
            .await
            .expect("a 401 is Platform Access's answer");
        assert_eq!(answer, Verification::Rejected(code));
    }
}

#[tokio::test]
async fn an_unknown_credential_does_not_authenticate() {
    let partners = PlatformAccessPartners::new(client(&platform_access(404, "NOT_FOUND")));
    assert_eq!(
        partners
            .credential_active(&scope(), Uuid::new_v4())
            .await
            .ok(),
        Some(false)
    );
    let partners =
        PlatformAccessPartners::new(client(&platform_access(403, "DELEGATED_CONTEXT_INVALID")));
    assert_eq!(
        partners
            .credential_active(&scope(), Uuid::new_v4())
            .await
            .ok(),
        Some(false)
    );
}

#[tokio::test]
async fn an_environment_without_origins_has_none() {
    let partners = PlatformAccessPartners::new(client(&platform_access(404, "NOT_FOUND")));
    assert_eq!(
        partners.environment_origins(Uuid::new_v4()).await.ok(),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn a_stale_delegated_scope_is_invalid_not_an_outage() {
    let authority =
        PlatformAccessAuthority::new(client(&platform_access(403, "DELEGATED_CONTEXT_INVALID")));
    let answer = authority
        .validate_delegated(&scope().delegated())
        .await
        .expect("a 403 DELEGATED_CONTEXT_INVALID is Platform Access's answer");
    assert_eq!(answer, DelegatedScope::Invalid);
}

#[tokio::test]
async fn a_real_outage_is_still_unavailable() {
    let partners =
        PlatformAccessPartners::new(client(&platform_access(503, "SERVICE_UNAVAILABLE")));
    assert!(partners.verify_user_token("cfut_sbx_x.y.z").await.is_err());
    assert!(partners
        .credential_active(&scope(), Uuid::new_v4())
        .await
        .is_err());
}
