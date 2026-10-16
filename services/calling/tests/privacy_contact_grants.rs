//! Part D §31: Calling mints Contact Vault access tokens and never receives a telephone
//! number. The grant needs the credential's own `privacy.contacts.*` scopes, is signed
//! with a DEDICATED key (never the service-auth key), and is accepted by Privacy Relay's
//! contact verifier only — never as a service token.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{platform::scopes::names, privacy_relay::contacts::ContactDelegationSettings};
use cf_service_auth::{
    ContactDelegationSigner, ContactDelegationTrust, ContactDelegationVerifier, ContactScope,
    KeyId, PresentedServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
};
use common::{calls::CallWorld, fake_media::FakeMediaProvider, Harness, TestDb};
use serde_json::json;

async fn world(configured: bool) -> Option<(CallWorld, Option<Arc<ContactDelegationSigner>>)> {
    let db = Arc::new(TestDb::new().await?);
    let media = Arc::new(FakeMediaProvider::default());
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let mut runtime = common::test_runtime(Some(media.clone()), bus.clone());
    let signer = configured.then(|| {
        Arc::new(
            ContactDelegationSigner::new(
                ServiceId::calling(),
                RuntimeEnvironment::Test,
                ServiceSigningKey::generate(KeyId::parse("calling-contact-test-1").unwrap()),
                120,
                Arc::new(cf_service_auth::SystemClock),
            )
            .unwrap(),
        )
    });
    runtime.contact_delegation = signer.clone();
    let h = Harness::with_runtime(
        db,
        calling::domain::ManualClock::new(common::t0()),
        media,
        bus,
        runtime,
    );
    Some((
        CallWorld::in_harness(h, "acme", calling::domain::EnvironmentKind::Production).await,
        signer,
    ))
}

fn verifier(signer: &ContactDelegationSigner) -> ContactDelegationVerifier {
    let trust = ContactDelegationTrust::from_json(
        &json!({
            "bundle_version": 1, "token_use": "CONTACT_VAULT_ACCESS", "audience": "cfmeet.privacy-relay", "runtime_environment": "test",
            "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": [signer.public_jwk()],
                "scopes": ["privacy.contacts.read", "privacy.contacts.write", "privacy.contacts.verify"] }],
        })
        .to_string(),
        &ServiceId::privacy_relay(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    ContactDelegationVerifier::new(trust, Arc::new(cf_service_auth::SystemClock))
}

const PATH: &str = "/v1/privacy-relay/contact-access-grants";

#[tokio::test]
async fn a_grant_carries_only_scopes_the_credential_holds_and_verifies_at_privacy_relay() {
    let Some((w, signer)) = world(true).await else {
        return;
    };
    let signer = signer.unwrap();
    let writer =
        w.h.issue(
            w.consumer.sa(w.kind),
            &[names::PRIVACY_CONTACTS_WRITE, names::PRIVACY_CONTACTS_READ],
        )
        .await;
    let secret = writer.secret.expose_once().to_string();
    let r =
        w.h.request(
            Method::POST,
            PATH,
            Some(&secret),
            Some(json!({ "scopes": ["privacy.contacts.write"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let grant = &r.json["grant"];
    assert_eq!(grant["scopes"], json!(["privacy.contacts.write"]));
    assert_eq!(
        (
            grant["tokenType"].as_str(),
            grant["authorizationHeader"].as_str(),
            grant["audience"].as_str()
        ),
        (
            Some("Bearer"),
            Some("X-CF-Contact-Authorization"),
            Some("cfmeet.privacy-relay")
        )
    );
    assert_eq!(r.headers["cache-control"], "no-store");
    let token = grant["accessToken"].as_str().unwrap();
    // Privacy Relay's contact verifier accepts it, with the credential's own scope.
    let access = verifier(&signer)
        .verify(&format!("Bearer {token}"))
        .unwrap();
    let scope = w.scope();
    assert_eq!(access.delegated().tenant_id(), scope.tenant_id().as_uuid());
    assert_eq!(
        access.delegated().application_environment_id(),
        scope.environment_id().as_uuid()
    );
    assert_eq!(
        access.delegated().initiating_service_account_id(),
        w.consumer.sa(w.kind).as_uuid()
    );
    assert!(access.has_scope(ContactScope::Write) && !access.has_scope(ContactScope::Read));
    assert_eq!(
        access.token_id().as_str(),
        grant["grantId"].as_str().unwrap()
    );
    // ...and it is never a service token.
    let presented = PresentedServiceRequest {
        service_authorization: &format!("Bearer {token}"),
        request_id: "req_0123456789abcdef0123456789abcdef",
        correlation_id: "corr_0123456789abcdef0123456789abcdef",
    };
    assert!(w.h.peers.privacy_relay_verifier.verify(&presented).is_err());
    // The customer credential never appears in the grant.
    assert!(!r.text.contains(&secret) && !r.text.contains(common::secret_part(&secret)));

    // A scope the credential lacks, an unknown scope, an empty set and any extra member.
    let r =
        w.h.request(
            Method::POST,
            PATH,
            Some(&secret),
            Some(json!({ "scopes": ["privacy.contacts.verify"] })),
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("INSUFFICIENT_SCOPE"))
    );
    for body in [
        json!({ "scopes": ["privacy.contacts.*"] }),
        json!({ "scopes": ["calls.write"] }),
        json!({ "scopes": [] }),
        json!({ "scopes": ["privacy.contacts.write", "privacy.contacts.write"] }),
        json!({ "scopes": ["privacy.contacts.write"], "telephoneNumber": "+39 02 1234 5678" }),
        json!({ "scopes": ["privacy.contacts.write"], "tenantId": "11111111-2222-4333-8444-555555555555" }),
    ] {
        let r =
            w.h.request(Method::POST, PATH, Some(&secret), Some(body.clone()))
                .await;
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR", "{body}");
        assert!(!r.text.contains("1234 5678"), "the number is never echoed");
    }
    // calls.* credentials cannot mint contact access.
    let r =
        w.h.request(
            Method::POST,
            PATH,
            Some(w.secret()),
            Some(json!({ "scopes": ["privacy.contacts.read"] })),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    let r =
        w.h.request(
            Method::POST,
            PATH,
            None,
            Some(json!({ "scopes": ["privacy.contacts.read"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn without_a_delegation_key_no_contact_access_is_possible() {
    let Some((w, _)) = world(false).await else {
        return;
    };
    let writer =
        w.h.issue(w.consumer.sa(w.kind), &[names::PRIVACY_CONTACTS_WRITE])
            .await;
    let r =
        w.h.request(
            Method::POST,
            PATH,
            Some(writer.secret.expose_once()),
            Some(json!({ "scopes": ["privacy.contacts.write"] })),
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("SERVICE_UNAVAILABLE"))
    );
}

#[test]
fn the_delegation_key_must_differ_from_the_service_key() {
    let dir = std::env::temp_dir().join(format!(
        "calling-contact-key-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let service = ServiceSigningKey::generate(KeyId::parse("calling-test-1").unwrap());
    let other = ServiceSigningKey::generate(KeyId::parse("calling-contact-test-1").unwrap());
    let write = |name: &str, key: &ServiceSigningKey| {
        let p = dir.join(name);
        std::fs::write(&p, key.to_pkcs8_pem().as_bytes()).unwrap();
        p.to_string_lossy().to_string()
    };
    let same = write("same.pem", &service);
    let different = write("other.pem", &other);
    let load = |kid: &str, file: &str| {
        let vars = [
            ("CONTACT_DELEGATION_KEY_ID", kid.to_string()),
            ("CONTACT_DELEGATION_PRIVATE_KEY_FILE", file.to_string()),
        ];
        ContactDelegationSettings::from_lookup(&|k: &str| {
            vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        })?
        .load(
            RuntimeEnvironment::Test,
            &service.public_jwk(),
            Arc::new(cf_service_auth::SystemClock),
        )
    };
    assert!(load("calling-contact-test-1", &different)
        .unwrap()
        .is_some());
    // Same key material under another kid, or the same kid: refused.
    assert!(load("calling-contact-test-9", &same).is_err());
    assert!(load("calling-test-1", &different).is_err());
    // Half a configuration, an inline key or an out-of-range TTL: refused.
    assert!(ContactDelegationSettings::from_lookup(
        &|k: &str| (k == "CONTACT_DELEGATION_KEY_ID").then(|| "k-1".to_string())
    )
    .is_err());
    assert!(ContactDelegationSettings::from_lookup(&|k: &str| (k
        == "CONTACT_DELEGATION_PRIVATE_KEY")
        .then(|| "x".to_string()))
    .is_err());
    assert!(ContactDelegationSettings::from_lookup(&|k: &str| (k
        == "CONTACT_DELEGATION_TTL_SECONDS")
        .then(|| "301".to_string()))
    .is_err());
    assert!(ContactDelegationSettings::from_lookup(&|_| None)
        .unwrap()
        .key
        .is_none());
    std::fs::remove_dir_all(&dir).ok();
}
