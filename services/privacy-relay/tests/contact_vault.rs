//! Part D §31: the Contact Vault boundary — canonicalization, authenticated encryption
//! bound to its owner, delegated access, versioned updates, audit, kill switch and
//! zero-downtime encryption-key rotation.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::{
    contact_delegation::CONTACT_AUTHORIZATION, ApplicationEnvironmentKind, ContactDelegationSigner,
    ContactScope, KeyId, ManualClock as AuthClock, OutboundServiceRequest, RuntimeEnvironment,
    ServiceId, ServiceSigningKey,
};
use common::{dump_database, scalar, test_vault_keys, Consumer, Harness};
use privacy_relay::{
    contact_vault::{
        crypto::{self, CryptoError},
        keyring::{
            activate_key_version, add_key_version, generate_keyring_json, FileKeyring, KeyPurpose,
            KeyVersion,
        },
        repository, VaultKeys,
    },
    scope::RelayScope,
};
use serde_json::{json, Value};
use uuid::Uuid;

const NUMBER: &str = "+39 02 1234 5678";
const E164: &str = "+390212345678";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

fn scope_of(c: &Consumer) -> RelayScope {
    RelayScope::of_delegated(&c.delegated())
}

async fn row(
    h: &Harness,
    c: &Consumer,
    id: &Value,
) -> privacy_relay::contact_vault::model::ContactRow {
    repository::find(
        h.conn(),
        scope_of(c),
        id.as_str().unwrap().parse().unwrap(),
        false,
    )
    .await
    .unwrap()
    .unwrap()
}

fn open(
    keys: &VaultKeys,
    r: &privacy_relay::contact_vault::model::ContactRow,
) -> Result<String, CryptoError> {
    crypto::open(keys.encryption(), r.scope(), r.id, &r.sealed().unwrap())
        .map(|n| format!("{n:?}").len().to_string())
}

/// The canonical value of a stored row (test-only decryption through the public crypto API).
fn decrypt(keys: &VaultKeys, r: &privacy_relay::contact_vault::model::ContactRow) -> String {
    let n = crypto::open(keys.encryption(), r.scope(), r.id, &r.sealed().unwrap()).unwrap();
    // CanonicalNumber has no Display; its masked display + country are public.
    format!("{}|{:?}", n.masked_display(), n.country_iso2())
}

#[tokio::test]
async fn a_number_is_canonicalized_encrypted_and_only_ever_returned_masked() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    // A keyring whose material this test knows (to prove it never reaches the database).
    let doc = generate_keyring_json(
        KeyPurpose::Encryption,
        &KeyVersion::parse("ck-test-1").unwrap(),
    );
    let key_b64 = serde_json::from_str::<Value>(&doc).unwrap()["keys"][0]["key"]
        .as_str()
        .unwrap()
        .to_string();
    let keys = h
        .vault_keys
        .with_encryption(Arc::new(
            FileKeyring::from_json(&doc, KeyPurpose::Encryption).unwrap(),
        ))
        .unwrap();
    h.set_vault_keys(keys);
    let c = consumer();
    let token = h.writer(&c);
    for (body, masked, country) in [
        (json!({ "telephoneNumber": NUMBER }), "+39••••••5678", "IT"),
        (
            json!({ "telephoneNumber": "(020) 7946 0958", "defaultCountry": "GB" }),
            "+44••••••0958",
            "GB",
        ),
    ] {
        let r = h
            .contact_call(
                Method::POST,
                "/v1/privacy-relay/contacts",
                Some(&token),
                Some(body),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let contact = &r.json["contact"];
        assert_eq!(contact["maskedDisplay"], masked);
        assert_eq!(contact["country"], country);
        assert_eq!(
            (
                contact["verificationState"].as_str(),
                contact["version"].as_i64()
            ),
            (Some("UNVERIFIED"), Some(1))
        );
        assert_eq!(contact["capabilities"]["voice"], "UNKNOWN");
        assert_eq!(r.headers["cache-control"], "no-store");
        let keys: Vec<&String> = contact.as_object().unwrap().keys().collect();
        for forbidden in [
            "telephoneNumber",
            "ciphertext",
            "nonce",
            "fingerprint",
            "encryptionKeyVersion",
        ] {
            assert!(!keys.iter().any(|k| *k == forbidden), "{forbidden}");
        }
        assert!(!r.text.contains("0212345678") && !r.text.contains("2079460958"));
        // Readable (masked) through GET.
        let path = format!(
            "/v1/privacy-relay/contacts/{}",
            contact["contactReference"].as_str().unwrap()
        );
        let got = h.contact_call(Method::GET, &path, Some(&token), None).await;
        assert_eq!(got.json["contact"], *contact);
    }
    // The stored row holds ciphertext only; decryption restores the canonical number.
    let created = h.create_contact(&c, NUMBER).await;
    let stored = row(&h, &c, &created["contactReference"]).await;
    assert_eq!(stored.sealed().unwrap().nonce.len(), 12);
    assert_eq!(stored.encryption_key_version, "ck-test-1");
    assert!(!stored.ciphertext.windows(10).any(|w| w == b"0212345678"));
    assert_eq!(
        decrypt(&h.vault_keys, &stored),
        "+39••••••5678|Some(\"IT\")"
    );
    let n = crypto::open(
        h.vault_keys.encryption(),
        stored.scope(),
        stored.id,
        &stored.sealed().unwrap(),
    )
    .unwrap();
    assert!(
        !format!("{n:?}").contains("0212345678"),
        "the canonical value never prints"
    );

    // Nothing in the database contains the number or any key material.
    let dump = dump_database(h.conn()).await;
    for needle in [E164, "0212345678", "02 1234 5678", "2079460958"] {
        assert!(!dump.contains(needle), "{needle} appears in the database");
    }
    use base64::Engine;
    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(&key_b64)
        .unwrap();
    assert!(!dump.contains(&key_b64), "key material in the database");
    assert!(
        !dump.contains(&hex::encode(&key_bytes)),
        "key bytes in the database"
    );
    // The only binary columns are the ciphertext, its nonce and the keyed fingerprint.
    let bytea = common::rows(
        h.conn(),
        "SELECT table_name || '.' || column_name FROM information_schema.columns WHERE table_schema = 'public' AND data_type = 'bytea' ORDER BY 1",
    )
    .await;
    assert_eq!(
        bytea,
        vec![
            vec!["contact_fingerprints.fingerprint".to_string()],
            vec!["protected_contacts.ciphertext".to_string()],
            vec!["protected_contacts.nonce".to_string()]
        ]
    );
}

#[tokio::test]
async fn invalid_and_unsupported_numbers_are_refused_without_echoing_them() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let token = h.writer(&c);
    for (body, status, code) in [
        (
            json!({ "telephoneNumber": "+39 02 1234 5678 ext 77" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "CONTACT_INVALID",
        ),
        (
            json!({ "telephoneNumber": "tel:+390212345678" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "CONTACT_INVALID",
        ),
        (
            json!({ "telephoneNumber": "02 1234 5678" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "CONTACT_INVALID",
        ),
        (
            json!({ "telephoneNumber": "+1 123 456 7890" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "CONTACT_INVALID",
        ),
        (
            json!({ "telephoneNumber": "02 1234 5678", "defaultCountry": "it" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "CONTACT_INVALID",
        ),
        // Client-asserted verification, scope or unknown members.
        (
            json!({ "telephoneNumber": NUMBER, "verificationState": "VERIFIED" }),
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
        ),
        (
            json!({ "telephoneNumber": NUMBER, "verified": true }),
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
        ),
        (
            json!({ "telephoneNumber": NUMBER, "tenantId": Uuid::new_v4() }),
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
        ),
        (
            json!({ "telephoneNumber": 390212345678_i64 }),
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
        ),
    ] {
        let r = h
            .contact_call(
                Method::POST,
                "/v1/privacy-relay/contacts",
                Some(&token),
                Some(body.clone()),
            )
            .await;
        assert_eq!((r.status, r.code()), (status, code), "{body}: {}", r.text);
        for needle in ["0212345678", "1234 5678", "390212345678", "4567890"] {
            assert!(!r.text.contains(needle), "{body} echoed in {}", r.text);
        }
    }
    // Malformed JSON quoting the number.
    let r = h
        .raw(
            axum::http::Request::builder()
                .method(Method::POST)
                .uri("/v1/privacy-relay/contacts")
                .header(CONTACT_AUTHORIZATION, format!("Bearer {token}")),
            None,
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        0
    );
    // Every refusal is audited (no number in the audit).
    assert!(scalar(h.conn(), "SELECT count(*) FROM contact_access_audit WHERE operation = 'CREATE' AND outcome = 'DENIED' AND reason_code = 'CONTACT_INVALID'").await >= 5);
}

#[tokio::test]
async fn ciphertext_is_bound_to_its_contact_and_scope() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (consumer(), consumer());
    let first = h.create_contact(&a, NUMBER).await;
    let second = h.create_contact(&a, "+44 20 7946 0958").await;
    let other_scope = h.create_contact(&b, NUMBER).await;
    let r1 = row(&h, &a, &first["contactReference"]).await;
    let r3 = row(&h, &b, &other_scope["contactReference"]).await;
    // Same number, two scopes: different nonces and ciphertexts.
    assert_ne!(r1.nonce, r3.nonce);
    assert_ne!(r1.ciphertext, r3.ciphertext);
    // Copy contact 1's ciphertext into contact 2 and into the other tenant's contact.
    for target in [
        second["contactReference"].as_str().unwrap(),
        other_scope["contactReference"].as_str().unwrap(),
    ] {
        common::exec(
            h.conn(),
            &format!(
                "UPDATE protected_contacts SET ciphertext = (SELECT ciphertext FROM protected_contacts WHERE id = '{0}'), \
                 nonce = (SELECT nonce FROM protected_contacts WHERE id = '{0}') WHERE id = '{target}'",
                r1.id
            ),
        )
        .await
        .unwrap();
    }
    let moved = row(&h, &a, &second["contactReference"]).await;
    let moved_scope = row(&h, &b, &other_scope["contactReference"]).await;
    assert_eq!(
        open(&h.vault_keys, &moved).unwrap_err(),
        CryptoError::Authentication
    );
    assert_eq!(
        open(&h.vault_keys, &moved_scope).unwrap_err(),
        CryptoError::Authentication
    );
    assert!(open(&h.vault_keys, &r1).is_ok());
}

#[tokio::test]
async fn only_a_valid_contact_vault_access_token_opens_the_contact_routes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let body = json!({ "telephoneNumber": NUMBER });
    let path = "/v1/privacy-relay/contacts";
    let expect_401 = |r: &common::Response, what: &str| {
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "CONTACT_DELEGATION_INVALID"),
            "{what}: {}",
            r.text
        );
        assert!(r.headers.get("www-authenticate").is_some());
    };
    // No token.
    expect_401(
        &h.contact_call(Method::POST, path, None, Some(body.clone()))
            .await,
        "none",
    );
    // A customer API credential, alone or alongside a valid token.
    let customer = axum::http::Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("authorization", "Bearer cfmk_live_abc_def");
    expect_401(
        &h.raw(customer, Some(body.clone())).await,
        "customer credential",
    );
    let both = axum::http::Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("authorization", "Bearer cfmk_live_abc_def")
        .header(CONTACT_AUTHORIZATION, format!("Bearer {}", h.writer(&c)));
    expect_401(
        &h.raw(both, Some(body.clone())).await,
        "customer credential + token",
    );
    // A service token (Calling's service key) in the contact header.
    let signed = h
        .ids
        .calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(c.delegated()),
        )
        .unwrap();
    let service = signed.token().expose_for_transport().to_string();
    expect_401(
        &h.contact_call(Method::POST, path, Some(&service), Some(body.clone()))
            .await,
        "service token",
    );
    // Wrong audience, wrong runtime environment, expired, untrusted key, not yet valid.
    let wall = Arc::new(cf_service_auth::SystemClock);
    let signer_with =
        |env: RuntimeEnvironment, key: ServiceSigningKey, clock: cf_service_auth::SharedClock| {
            ContactDelegationSigner::new(ServiceId::calling(), env, key, 120, clock).unwrap()
        };
    let audience = h
        .contact_signer
        .sign(
            &ServiceId::telephony(),
            &c.delegated(),
            &[ContactScope::Write],
        )
        .unwrap();
    expect_401(
        &h.contact_call(
            Method::POST,
            path,
            Some(audience.expose_for_delivery()),
            Some(body.clone()),
        )
        .await,
        "audience",
    );
    let untrusted = signer_with(
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("calling-contact-test-1").unwrap()),
        wall.clone(),
    );
    let t = untrusted
        .sign(
            &ServiceId::privacy_relay(),
            &c.delegated(),
            &[ContactScope::Write],
        )
        .unwrap();
    expect_401(
        &h.contact_call(
            Method::POST,
            path,
            Some(t.expose_for_delivery()),
            Some(body.clone()),
        )
        .await,
        "forged key",
    );
    let staging = signer_with(
        RuntimeEnvironment::Staging,
        ServiceSigningKey::generate(KeyId::parse("calling-contact-staging-9").unwrap()),
        wall.clone(),
    );
    let t = staging
        .sign(
            &ServiceId::privacy_relay(),
            &c.delegated(),
            &[ContactScope::Write],
        )
        .unwrap();
    expect_401(
        &h.contact_call(
            Method::POST,
            path,
            Some(t.expose_for_delivery()),
            Some(body.clone()),
        )
        .await,
        "environment",
    );
    // Expired: a token minted 10 minutes ago by the trusted key (verifier uses the wall clock).
    let past = AuthClock::new(chrono::Utc::now().timestamp() - 600);
    let pem = h.ids.contact_pem.clone();
    let old = signer_with(
        RuntimeEnvironment::Test,
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse("calling-contact-test-1").unwrap(), &pem)
            .unwrap(),
        Arc::new(past),
    );
    let t = old
        .sign(
            &ServiceId::privacy_relay(),
            &c.delegated(),
            &[ContactScope::Write],
        )
        .unwrap();
    expect_401(
        &h.contact_call(
            Method::POST,
            path,
            Some(t.expose_for_delivery()),
            Some(body.clone()),
        )
        .await,
        "expired",
    );
    // A valid token without the needed scope.
    let reader = h.contact_token(&c, &[ContactScope::Read]);
    let r = h
        .contact_call(Method::POST, path, Some(&reader), Some(body.clone()))
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "CONTACT_SCOPE_DENIED")
    );
    // The valid token works.
    let ok = h
        .contact_call(Method::POST, path, Some(&h.writer(&c)), Some(body.clone()))
        .await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);

    // A contact token can never open the internal API.
    let token = h.writer(&c);
    let internal = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/internal/v1/contexts/query")
        .header("x-cf-service-authorization", format!("Bearer {token}"))
        .header("x-cf-request-id", "req_0123456789abcdef0123456789abcdef")
        .header(
            "x-cf-correlation-id",
            "corr_0123456789abcdef0123456789abcdef",
        );
    let r = h.raw(internal, Some(json!({}))).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED")
    );
    let r = h
        .contact_call(
            Method::POST,
            "/internal/v1/contexts/query",
            Some(&token),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED");
    // Nothing was stored by the refused requests.
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        1
    );
}

#[tokio::test]
async fn updates_are_versioned_reset_verification_and_foreign_references_are_not_found() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (consumer(), consumer());
    let contact = h.create_contact(&a, NUMBER).await;
    let id = contact["contactReference"].as_str().unwrap();
    let path = format!("/v1/privacy-relay/contacts/{id}");
    let token = h.writer(&a);
    // Mark it verified in storage (§33 adds the trusted path) to prove the reset.
    common::exec(
        h.conn(),
        &format!("UPDATE protected_contacts SET verification_state = 'VERIFIED' WHERE id = '{id}'"),
    )
    .await
    .unwrap();
    let r = h
        .contact_call(
            Method::PATCH,
            &path,
            Some(&token),
            Some(json!({ "expectedVersion": 1, "telephoneNumber": "+44 20 7946 0958" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let updated = &r.json["contact"];
    assert_eq!(updated["contactReference"], id, "same ContactReference");
    assert_eq!(
        (
            updated["version"].as_i64(),
            updated["verificationState"].as_str()
        ),
        (Some(2), Some("UNVERIFIED"))
    );
    assert_eq!(updated["maskedDisplay"], "+44••••••0958");
    assert_eq!(
        decrypt(
            &h.vault_keys,
            &row(&h, &a, &contact["contactReference"]).await
        ),
        "+44••••••0958|Some(\"GB\")"
    );
    // Stale version.
    let r = h
        .contact_call(
            Method::PATCH,
            &path,
            Some(&token),
            Some(json!({ "expectedVersion": 1, "telephoneNumber": NUMBER })),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "CONTACT_VERSION_CONFLICT")
    );
    // Foreign and unknown references are the same 404, for every operation.
    let foreign = h.writer(&b);
    let unknown = format!("/v1/privacy-relay/contacts/{}", Uuid::new_v4());
    for (method, p, body) in [
        (Method::GET, path.clone(), None),
        (
            Method::PATCH,
            path.clone(),
            Some(json!({ "expectedVersion": 2, "telephoneNumber": NUMBER })),
        ),
        (
            Method::POST,
            format!("{path}/disable"),
            Some(json!({ "expectedVersion": 2 })),
        ),
    ] {
        let theirs = h
            .contact_call(method.clone(), &p, Some(&foreign), body.clone())
            .await;
        let missing_path = p.replace(id, unknown.rsplit('/').next().unwrap());
        let missing = h
            .contact_call(method.clone(), &missing_path, Some(&foreign), body)
            .await;
        assert_eq!(
            (theirs.status, theirs.code()),
            (StatusCode::NOT_FOUND, "CONTACT_NOT_FOUND"),
            "{method} {p}"
        );
        assert_eq!(theirs.normalized_error(), missing.normalized_error());
    }
    let r = h
        .contact_call(
            Method::GET,
            "/v1/privacy-relay/contacts/not-a-uuid",
            Some(&token),
            None,
        )
        .await;
    assert_eq!(r.code(), "CONTACT_NOT_FOUND");
    // Disable: restrictive, idempotent; a disabled contact cannot be changed.
    let r = h
        .contact_call(
            Method::POST,
            &format!("{path}/disable"),
            Some(&token),
            Some(json!({ "expectedVersion": 2 })),
        )
        .await;
    assert_eq!(
        (
            r.status,
            r.json["contact"]["status"].as_str(),
            r.json["contact"]["version"].as_i64()
        ),
        (StatusCode::OK, Some("DISABLED"), Some(3))
    );
    let again = h
        .contact_call(
            Method::POST,
            &format!("{path}/disable"),
            Some(&token),
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(again.json["contact"]["version"], 3);
    let r = h
        .contact_call(
            Method::PATCH,
            &path,
            Some(&token),
            Some(json!({ "expectedVersion": 3, "telephoneNumber": NUMBER })),
        )
        .await;
    assert_eq!(r.code(), "CONTACT_DISABLED");
}

#[tokio::test]
async fn every_sensitive_operation_is_audited_append_only_and_without_the_number() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (consumer(), consumer());
    let contact = h.create_contact(&a, NUMBER).await;
    let id = contact["contactReference"].as_str().unwrap();
    let token = h.writer(&a);
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
        Some(json!({ "expectedVersion": 1, "telephoneNumber": "+44 20 7946 0958" })),
    )
    .await;
    h.contact_call(
        Method::GET,
        &format!("/v1/privacy-relay/contacts/{id}"),
        Some(&h.writer(&b)),
        None,
    )
    .await;
    let rows = common::rows(
        h.conn(),
        &format!("SELECT operation, outcome, coalesce(reason_code, ''), actor_kind, actor_service, (credential_ref IS NOT NULL)::text, \
                  service_account_id::text, request_id, correlation_id FROM contact_access_audit WHERE contact_reference = '{id}' ORDER BY created_at, operation"),
    )
    .await;
    let ops: Vec<(String, String, String)> = rows
        .iter()
        .map(|r| (r[0].clone(), r[1].clone(), r[2].clone()))
        .collect();
    assert!(
        ops.contains(&("CREATE".into(), "SUCCEEDED".into(), "".into())),
        "{ops:?}"
    );
    assert!(ops.contains(&("READ".into(), "SUCCEEDED".into(), "".into())));
    assert!(ops.contains(&("UPDATE".into(), "SUCCEEDED".into(), "".into())));
    assert!(
        ops.contains(&("READ".into(), "DENIED".into(), "CONTACT_NOT_FOUND".into())),
        "the foreign attempt is audited"
    );
    for r in &rows {
        assert_eq!(
            (r[3].as_str(), r[4].as_str(), r[5].as_str()),
            ("DELEGATED_CLIENT", "cfmeet.calling", "true")
        );
        assert!(r[7].starts_with("req_") && r[8].starts_with("corr_"));
    }
    // The foreign attempt is recorded in the CALLER's scope, never in the owner's.
    assert_eq!(scalar(h.conn(), &format!("SELECT count(*) FROM contact_access_audit WHERE tenant_id = '{}' AND outcome = 'DENIED'", a.tenant_id)).await, 0);
    let audit = common::dump_table(h.conn(), "contact_access_audit").await;
    for needle in [E164, "0212345678", "2079460958"] {
        assert!(!audit.contains(needle));
    }
    for sql in [
        "UPDATE contact_access_audit SET outcome = 'DENIED'",
        "DELETE FROM contact_access_audit",
    ] {
        assert!(common::exec(h.conn(), sql).await.is_err(), "{sql}");
    }
    assert!(common::exec(h.conn(), "DELETE FROM protected_contacts")
        .await
        .is_err());
    assert!(common::exec(
        h.conn(),
        &format!(
            "UPDATE protected_contacts SET tenant_id = '{}' WHERE id = '{id}'",
            Uuid::new_v4()
        )
    )
    .await
    .is_err());
}

#[tokio::test]
async fn the_kill_switch_and_missing_configuration_fail_closed() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let contact = h.create_contact(&c, NUMBER).await;
    let id = contact["contactReference"].as_str().unwrap();
    let token = h.writer(&c);
    h.contact_feature.set(false);
    let r = h
        .contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&token),
            Some(json!({ "telephoneNumber": NUMBER })),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::SERVICE_UNAVAILABLE, "CONTACT_VAULT_DISABLED")
    );
    let r = h
        .contact_call(
            Method::PATCH,
            &format!("/v1/privacy-relay/contacts/{id}"),
            Some(&token),
            Some(json!({ "expectedVersion": 1, "telephoneNumber": NUMBER })),
        )
        .await;
    assert_eq!(r.code(), "CONTACT_VAULT_DISABLED");
    // Restrictive operations keep working.
    let r = h
        .contact_call(
            Method::POST,
            &format!("/v1/privacy-relay/contacts/{id}/disable"),
            Some(&token),
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    h.contact_feature.set(true);
    // A vault without key material never stores anything (no plaintext fallback).
    let unconfigured = h.vault_with(None);
    h.contacts = unconfigured.clone();
    h.state = h
        .state
        .clone()
        .with_contact_vault(unconfigured, Some(h.ids.contact_verifier()));
    let r = h
        .contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&token),
            Some(json!({ "telephoneNumber": NUMBER })),
        )
        .await;
    assert_eq!(r.code(), "CONTACT_VAULT_DISABLED");
    // No delegation trust configured: every contact route is closed.
    h.state = privacy_relay::state::AppState::new(h.relay.clone(), h.ids.runtime.clone());
    let r = h
        .contact_call(
            Method::GET,
            &format!("/v1/privacy-relay/contacts/{id}"),
            Some(&token),
            None,
        )
        .await;
    assert_eq!(r.code(), "CONTACT_VAULT_DISABLED");
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        1
    );
}

#[tokio::test]
async fn encryption_key_rotation_reencrypts_in_batches_without_changing_any_contact() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let v1 = KeyVersion::parse("ck-v1").unwrap();
    let v2 = KeyVersion::parse("ck-v2").unwrap();
    let doc1 = generate_keyring_json(KeyPurpose::Encryption, &v1);
    let base = test_vault_keys("unused-e", "fk-rot-1");
    let keys = |doc: &str| {
        base.with_encryption(Arc::new(
            FileKeyring::from_json(doc, KeyPurpose::Encryption).unwrap(),
        ))
        .unwrap()
    };
    h.set_vault_keys(keys(&doc1));
    let c = consumer();
    let numbers = ["+39 02 1234 5678", "+44 20 7946 0958", "+234 803 123 4567"];
    let mut before = Vec::new();
    for n in numbers {
        before.push(h.create_contact(&c, n).await);
    }
    // Step 1-2: add V2 (V1 stays readable) and make it active for writes.
    let doc2 = activate_key_version(
        &add_key_version(&doc1, KeyPurpose::Encryption, &v2).unwrap(),
        KeyPurpose::Encryption,
        &v2,
    )
    .unwrap();
    h.set_vault_keys(keys(&doc2));
    let newer = h.create_contact(&c, "+1 650 253 0000").await;
    assert_eq!(
        row(&h, &c, &newer["contactReference"])
            .await
            .encryption_key_version,
        "ck-v2"
    );
    assert_eq!(
        h.contacts.key_version_counts().await.unwrap(),
        vec![("ck-v1".into(), 3), ("ck-v2".into(), 1)]
    );
    // Existing V1 rows still read (and decrypt) during the overlap.
    assert_eq!(
        decrypt(
            &h.vault_keys,
            &row(&h, &c, &before[0]["contactReference"]).await
        ),
        "+39••••••5678|Some(\"IT\")"
    );
    // Step 3: bounded, concurrent, resumable batches.
    let other = h.vault_with(Some(h.vault_keys.clone()));
    let (x, y) = tokio::join!(h.contacts.rewrap_batch(1), other.rewrap_batch(1));
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!(
        x.rewrapped + y.rewrapped,
        2,
        "SKIP LOCKED: disjoint batches"
    );
    let last = h.contacts.rewrap_batch(10).await.unwrap();
    assert_eq!((last.rewrapped, last.remaining), (1, 0));
    assert_eq!(
        h.contacts.rewrap_batch(10).await.unwrap().rewrapped,
        0,
        "idempotent once done"
    );
    // Step 4: verify — every contact is under V2, unchanged and decryptable WITHOUT V1.
    assert_eq!(
        h.contacts.key_version_counts().await.unwrap(),
        vec![("ck-v2".into(), 4)]
    );
    let mut only_v2: Value = serde_json::from_str(&doc2).unwrap();
    only_v2["keys"]
        .as_array_mut()
        .unwrap()
        .retain(|k| k["version"] == "ck-v2");
    let v2_only = keys(&only_v2.to_string());
    let token = h.writer(&c);
    for (i, b) in before.iter().enumerate() {
        let r = h
            .contact_call(
                Method::GET,
                &format!(
                    "/v1/privacy-relay/contacts/{}",
                    b["contactReference"].as_str().unwrap()
                ),
                Some(&token),
                None,
            )
            .await;
        assert_eq!(
            &r.json["contact"], b,
            "reference, version and safe metadata unchanged"
        );
        let stored = row(&h, &c, &b["contactReference"]).await;
        assert_eq!(
            decrypt(&v2_only, &stored),
            decrypt(&h.vault_keys, &stored),
            "{}",
            numbers[i]
        );
    }
    assert_eq!(scalar(h.conn(), "SELECT count(*) FROM contact_access_audit WHERE operation = 'REENCRYPT' AND outcome = 'SUCCEEDED' AND actor_kind = 'SYSTEM'").await, 3);
    // The worker sweep is a no-op now.
    h.contacts.run_sweep().await.unwrap();
}
