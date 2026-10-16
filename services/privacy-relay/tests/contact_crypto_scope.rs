//! §73 Contact Vault cross-scope cryptographic certification.
//!
//! A stored ciphertext opens only for the exact owner it was sealed for — tenant,
//! application, environment, ContactReference and key version (the AAD of
//! `contact_vault::crypto`). A ciphertext + nonce copied into another row fails
//! authentication on every path that decrypts: the crypto API, Telephony's
//! `resolve-for-execution` (audited FAILED, grant not consumed, no plaintext) and the
//! re-encryption worker (audited FAILED, row untouched); so does a whole row moved into
//! another tenant / application / environment with its ContactReference intact. A key version the keyring does
//! not hold refuses service start (the referenced-version guard `main` runs) and, if an
//! instance runs with such a keyring anyway, resolution fails closed without plaintext.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind;
use common::{at, iso, rows, scalar, Consumer, Harness};
use privacy_relay::{
    contact_vault::{
        crypto::{self, CryptoError},
        keyring::{
            activate_key_version, add_key_version, generate_keyring_json, FileKeyring, KeyPurpose,
            KeyVersion,
        },
        model::ContactRow,
        repository,
        service::RotationGuardError,
        VaultKeys,
    },
    scope::RelayScope,
};
use serde_json::{json, Value};
use uuid::Uuid;

const SOURCE: &str = "+44 20 7946 0123";
const SOURCE_E164: &str = "+442079460123";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

/// Another application of the SAME tenant.
fn sibling_application(c: &Consumer) -> Consumer {
    Consumer {
        application_id: Uuid::new_v4(),
        environment_id: Uuid::new_v4(),
        service_account_id: Uuid::new_v4(),
        ..c.clone()
    }
}

/// The harness keys with the encryption keyring `doc`.
fn with_encryption(base: &VaultKeys, doc: &str) -> VaultKeys {
    base.with_encryption(Arc::new(
        FileKeyring::from_json(doc, KeyPurpose::Encryption).unwrap(),
    ))
    .unwrap()
}

/// A verified contact of `c` with everything Telephony needs to resolve it: a context,
/// a PUBLIC_RELAY session, an AUTHORIZED decision and an ISSUED grant for one leg.
struct Ready {
    c: Consumer,
    contact: Uuid,
    grant: Value,
}

async fn ready(h: &Harness, c: &Consumer, number: &str) -> Ready {
    let contact = h.create_contact(c, number).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(c, &contact, "VERIFIED", Some(iso(at(50))))
        .await;
    ready_for(h, c, &contact).await
}

/// [`ready`] for an existing (verified) contact of `c`.
async fn ready_for(h: &Harness, c: &Consumer, contact: &str) -> Ready {
    let principal = Uuid::new_v4();
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(c, req).await;
    let session = h
        .create_session(
            c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let d = h
        .post(
            "/internal/v1/communication/authorize",
            c,
            json!({ "relaySessionId": session, "source": { "principalId": principal },
                    "targets": [{ "contactReferenceId": contact }], "channel": "VOICE",
                    "route": "PUBLIC_RELAY", "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(d.status, StatusCode::OK, "{}", d.text);
    let g = h
        .post(
            &format!("/internal/v1/contacts/{contact}/resolution-grants"),
            c,
            json!({ "relaySessionId": session, "decisionId": d.json["decisionId"],
                    "callId": Uuid::new_v4(), "callLegId": Uuid::new_v4(),
                    "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
        )
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    Ready {
        c: c.clone(),
        contact: contact.parse().unwrap(),
        grant: g.json["grant"].clone(),
    }
}

/// Telephony exchanges the grant (a fresh execution attempt each time).
async fn resolve(h: &Harness, r: &Ready) -> common::Response {
    let telephony = h.ids.telephony.clone();
    h.send_as(
        &telephony,
        Method::POST,
        &format!("/internal/v1/contacts/{}/resolve-for-execution", r.contact),
        Some(&r.c),
        Some(json!({
            "grantId": r.grant["grantId"], "grantSecret": r.grant["grantSecret"],
            "callId": r.grant["callId"], "callLegId": r.grant["callLegId"],
            "executionAttemptId": Uuid::new_v4(),
        })),
    )
    .await
}

async fn stored(h: &Harness, r: &Ready) -> ContactRow {
    repository::find(
        h.conn(),
        RelayScope::of_delegated(&r.c.delegated()),
        r.contact,
        false,
    )
    .await
    .unwrap()
    .unwrap()
}

async fn grant_status(h: &Harness, r: &Ready) -> String {
    rows(
        h.conn(),
        &format!(
            "SELECT status FROM contact_resolution_grants WHERE id = '{}'",
            r.grant["grantId"].as_str().unwrap()
        ),
    )
    .await[0][0]
        .clone()
}

async fn audited(h: &Harness, contact: Uuid, operation: &str, outcome: &str) -> i64 {
    scalar(
        h.conn(),
        &format!(
            "SELECT count(*) FROM contact_access_audit WHERE contact_reference = '{contact}' \
             AND operation = '{operation}' AND outcome = '{outcome}' \
             AND (outcome <> 'FAILED' OR reason_code = 'INTERNAL_ERROR')"
        ),
    )
    .await
}

/// Moves a contact row, ContactReference and ciphertext intact, into `to`'s scope — what
/// only an actor with direct database write access could do (the scope columns are
/// immutable and foreign-keyed, so the triggers are bypassed for this one statement).
async fn rehome(h: &Harness, contact: Uuid, to: &Consumer) {
    common::exec(
        h.conn(),
        &format!(
            "BEGIN; SET LOCAL session_replication_role = replica; \
             UPDATE protected_contacts SET tenant_id = '{}', application_id = '{}', environment_id = '{}' WHERE id = '{contact}'; \
             COMMIT;",
            to.tenant_id, to.application_id, to.environment_id
        ),
    )
    .await
    .unwrap();
}

/// The resolution of `r` fails closed: 500 INTERNAL_ERROR, no resolution body, none of
/// `digits` in the answer, the grant still ISSUED (nothing consumed).
async fn assert_fails_closed(h: &Harness, r: &Ready, digits: &[&str], what: &str) {
    let resp = resolve(h, r).await;
    assert_eq!(
        (resp.status, resp.code()),
        (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR"),
        "{what}: {}",
        resp.text
    );
    assert!(resp.json.get("resolution").is_none(), "{what}");
    for d in digits {
        assert!(
            !resp.text.contains(d),
            "{what}: plaintext {d} in the answer"
        );
    }
    assert_eq!(grant_status(h, r).await, "ISSUED", "{what}: grant consumed");
}

#[tokio::test]
async fn a_ciphertext_copied_across_tenant_application_environment_or_contact_never_opens() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let v1 = KeyVersion::parse("ck-scope-1").unwrap();
    let doc1 = generate_keyring_json(KeyPurpose::Encryption, &v1);
    let base = h.vault_keys.clone();
    h.set_vault_keys(with_encryption(&base, &doc1));

    let a = consumer();
    let source = ready(&h, &a, SOURCE).await;
    // One target per AAD scope part, each differing from the source in EXACTLY that part
    // (and, necessarily, the ContactReference); plus another contact of the same scope.
    let targets = [
        (
            "another tenant",
            consumer(),
            "+44 20 7946 0124",
            "+442079460124",
        ),
        (
            "another application of the same tenant",
            sibling_application(&a),
            "+44 20 7946 0125",
            "+442079460125",
        ),
        (
            "another environment of the same application",
            a.sibling_environment(),
            "+44 20 7946 0126",
            "+442079460126",
        ),
        (
            "another ContactReference of the same scope",
            a.clone(),
            "+44 20 7946 0127",
            "+442079460127",
        ),
    ];
    let mut prepared = Vec::new();
    for (what, c, number, e164) in &targets {
        let r = ready(&h, c, number).await;
        let original = stored(&h, &r).await;
        prepared.push((*what, r, original, *e164));
    }
    // The scope relations are what each case claims.
    let s = stored(&h, &source).await.scope();
    let scopes: Vec<RelayScope> = prepared.iter().map(|(_, _, o, _)| o.scope()).collect();
    assert_ne!(scopes[0].tenant_id(), s.tenant_id());
    assert_eq!(
        (
            scopes[1].tenant_id(),
            scopes[1].environment_id() != s.environment_id()
        ),
        (s.tenant_id(), true)
    );
    assert_ne!(scopes[1].application_id(), s.application_id());
    assert_eq!(
        (scopes[2].tenant_id(), scopes[2].application_id()),
        (s.tenant_id(), s.application_id())
    );
    assert_ne!(scopes[2].environment_id(), s.environment_id());
    assert_eq!(scopes[3], s);

    // Copy the source's ciphertext + nonce into every target row (same key version).
    for (_, r, original, _) in &prepared {
        assert_eq!(
            original.encryption_key_version.as_deref(),
            Some("ck-scope-1")
        );
        common::exec(
            h.conn(),
            &format!(
                "UPDATE protected_contacts SET ciphertext = (SELECT ciphertext FROM protected_contacts WHERE id = '{0}'), \
                 nonce = (SELECT nonce FROM protected_contacts WHERE id = '{0}') WHERE id = '{1}'",
                source.contact, r.contact
            ),
        )
        .await
        .unwrap();
    }
    let digits = [
        "2079460123",
        "2079460124",
        "2079460125",
        "2079460126",
        "2079460127",
    ];
    for (what, r, _, _) in &prepared {
        let moved = stored(&h, r).await;
        // 1. The crypto API.
        assert_eq!(
            crypto::open(
                h.vault_keys.encryption(),
                moved.scope(),
                moved.id,
                &moved.sealed().unwrap()
            )
            .unwrap_err(),
            CryptoError::Authentication,
            "{what}"
        );
        // 2. Telephony's resolution: fails closed, audited FAILED, nothing consumed.
        assert_fails_closed(&h, r, &digits, what).await;
        assert_eq!(
            audited(&h, r.contact, "RESOLVE_FOR_EXECUTION", "FAILED").await,
            1,
            "{what}"
        );
        assert_eq!(
            audited(&h, r.contact, "RESOLVE_FOR_EXECUTION", "SUCCEEDED").await,
            0,
            "{what}"
        );
    }
    // The source itself still opens.
    let src = stored(&h, &source).await;
    assert!(crypto::open(
        h.vault_keys.encryption(),
        src.scope(),
        src.id,
        &src.sealed().unwrap()
    )
    .is_ok());

    // 3. The re-encryption worker (key rotation) cannot open a moved ciphertext either:
    //    it re-encrypts the source only, audits every moved row FAILED and leaves it as is.
    let v2 = KeyVersion::parse("ck-scope-2").unwrap();
    let doc2 = activate_key_version(
        &add_key_version(&doc1, KeyPurpose::Encryption, &v2).unwrap(),
        KeyPurpose::Encryption,
        &v2,
    )
    .unwrap();
    h.set_vault_keys(with_encryption(&base, &doc2));
    let report = h.contacts.rewrap_batch(10).await.unwrap();
    assert_eq!(
        (report.rewrapped, report.failed, report.remaining),
        (1, 4, 4)
    );
    for (what, r, _, _) in &prepared {
        assert_eq!(
            audited(&h, r.contact, "REENCRYPT", "FAILED").await,
            1,
            "{what}"
        );
        assert_eq!(
            audited(&h, r.contact, "REENCRYPT", "SUCCEEDED").await,
            0,
            "{what}"
        );
        let after = stored(&h, r).await;
        assert_eq!(
            (
                after.encryption_key_version.as_deref(),
                &after.ciphertext,
                &after.nonce
            ),
            (Some("ck-scope-1"), &src.ciphertext, &src.nonce),
            "{what}: a row that does not open is never rewritten"
        );
    }

    // Control: with its OWN ciphertext back, every target resolves (same grant, the
    // failed attempts consumed nothing) — the copy alone caused each refusal.
    for (what, r, original, e164) in &prepared {
        common::exec(
            h.conn(),
            &format!(
                "UPDATE protected_contacts SET ciphertext = decode('{}', 'hex'), nonce = decode('{}', 'hex') WHERE id = '{}'",
                hex::encode(original.ciphertext.as_ref().unwrap()),
                hex::encode(original.nonce.as_ref().unwrap()),
                r.contact
            ),
        )
        .await
        .unwrap();
        let ok = resolve(&h, r).await;
        assert_eq!(ok.status, StatusCode::OK, "{what}: {}", ok.text);
        assert_eq!(ok.json["resolution"]["e164"], *e164, "{what}");
        assert_eq!(grant_status(&h, r).await, "CONSUMED", "{what}");
    }
    let ok = resolve(&h, &source).await;
    assert_eq!(ok.json["resolution"]["e164"], SOURCE_E164, "{}", ok.text);

    // No number reached the database (audit rows included).
    let dump = common::dump_database(h.conn()).await;
    for d in digits {
        assert!(!dump.contains(d), "{d} appears in the database");
    }
}

#[tokio::test]
async fn a_row_rehomed_to_another_scope_with_its_contact_reference_intact_never_opens() {
    // Only the scope differs here (same row, same ContactReference, same ciphertext, nonce
    // and key version): the tenant / application / environment parts of the AAD alone
    // must refuse it.
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = consumer();
    // Each target differs from `a` in EXACTLY one scope part (the other two ids kept), so
    // every AAD part is shown to bind on its own.
    let cases = [
        (
            "only the tenant differs",
            Consumer {
                tenant_id: Uuid::new_v4(),
                service_account_id: Uuid::new_v4(),
                ..a.clone()
            },
            "+44 20 7946 0131",
            "2079460131",
        ),
        (
            "only the application differs",
            Consumer {
                application_id: Uuid::new_v4(),
                service_account_id: Uuid::new_v4(),
                ..a.clone()
            },
            "+44 20 7946 0132",
            "2079460132",
        ),
        (
            "only the environment differs",
            a.sibling_environment(),
            "+44 20 7946 0133",
            "2079460133",
        ),
    ];
    for (what, to, number, digits) in &cases {
        let contact = h.create_contact(&a, number).await["contactReference"]
            .as_str()
            .unwrap()
            .to_string();
        h.verify_contact(&a, &contact, "VERIFIED", Some(iso(at(50))))
            .await;
        let id: Uuid = contact.parse().unwrap();
        let before = repository::find(
            h.conn(),
            RelayScope::of_delegated(&a.delegated()),
            id,
            false,
        )
        .await
        .unwrap()
        .unwrap();
        rehome(&h, id, to).await;
        let r = ready_for(&h, to, &contact).await;
        let moved = stored(&h, &r).await;
        assert_eq!(
            (
                moved.id,
                &moved.ciphertext,
                &moved.nonce,
                &moved.encryption_key_version
            ),
            (
                before.id,
                &before.ciphertext,
                &before.nonce,
                &before.encryption_key_version
            ),
            "{what}: only the scope moved"
        );
        assert_eq!(
            crypto::open(
                h.vault_keys.encryption(),
                moved.scope(),
                moved.id,
                &moved.sealed().unwrap()
            )
            .unwrap_err(),
            CryptoError::Authentication,
            "{what}"
        );
        assert_fails_closed(&h, &r, &[digits], what).await;
        assert_eq!(
            audited(&h, id, "RESOLVE_FOR_EXECUTION", "FAILED").await,
            1,
            "{what}"
        );
        assert_eq!(
            audited(&h, id, "RESOLVE_FOR_EXECUTION", "SUCCEEDED").await,
            0,
            "{what}"
        );
        // Control: back in its own scope, the very same row resolves.
        rehome(&h, id, &a).await;
        let home = ready_for(&h, &a, &contact).await;
        let ok = resolve(&h, &home).await;
        assert_eq!(ok.status, StatusCode::OK, "{what}: {}", ok.text);
        assert_eq!(
            ok.json["resolution"]["e164"],
            format!("+44{digits}"),
            "{what}"
        );
    }
}

#[tokio::test]
async fn a_key_version_missing_from_the_keyring_refuses_start_and_fails_closed_at_runtime() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let live = KeyVersion::parse("ck-live-1").unwrap();
    let doc = generate_keyring_json(KeyPurpose::Encryption, &live);
    let base = h.vault_keys.clone();
    h.set_vault_keys(with_encryption(&base, &doc));
    let r = ready(&h, &consumer(), SOURCE).await;
    let digits = ["2079460123"];
    h.contacts.verify_referenced_key_versions().await.unwrap();

    // A keyring that lost the referenced version: the start-up guard refuses it.
    let lost = with_encryption(
        &base,
        &generate_keyring_json(
            KeyPurpose::Encryption,
            &KeyVersion::parse("ck-live-2").unwrap(),
        ),
    );
    let refused = h
        .vault_with(Some(lost.clone()))
        .verify_referenced_key_versions()
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, RotationGuardError::Unreadable { purpose: "CONTACT_ENCRYPTION", version } if version == "ck-live-1"),
        "{refused}"
    );
    assert_eq!(
        h.vault_with(Some(lost.clone()))
            .rotation_status()
            .await
            .unwrap()
            .unreadable_encryption,
        vec!["ck-live-1".to_string()]
    );
    // ...and so does a fingerprint keyring that lost its referenced version.
    let lost_fingerprint = h
        .vault_keys
        .with_fingerprint(Arc::new(
            FileKeyring::from_json(
                &generate_keyring_json(
                    KeyPurpose::Fingerprint,
                    &KeyVersion::parse("fk-other").unwrap(),
                ),
                KeyPurpose::Fingerprint,
            )
            .unwrap(),
        ))
        .unwrap();
    let refused = h
        .vault_with(Some(lost_fingerprint))
        .verify_referenced_key_versions()
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, RotationGuardError::Unreadable { purpose: "CONTACT_FINGERPRINT", version } if version == "fk-test-1"),
        "{refused}"
    );

    // Runtime 1: an instance running with that keyring anyway never yields plaintext.
    h.set_vault_keys(lost);
    let row = stored(&h, &r).await;
    assert_eq!(
        crypto::open(
            h.vault_keys.encryption(),
            row.scope(),
            row.id,
            &row.sealed().unwrap()
        )
        .unwrap_err(),
        CryptoError::UnknownKeyVersion
    );
    assert_fails_closed(&h, &r, &digits, "keyring lost the version").await;
    assert_eq!(
        audited(&h, r.contact, "RESOLVE_FOR_EXECUTION", "FAILED").await,
        1
    );

    // Runtime 2: the right keyring, but the row names a version absent from it.
    h.set_vault_keys(with_encryption(&base, &doc));
    common::exec(
        h.conn(),
        &format!(
            "UPDATE protected_contacts SET encryption_key_version = 'ck-absent' WHERE id = '{}'",
            r.contact
        ),
    )
    .await
    .unwrap();
    assert!(matches!(
        h.contacts.verify_referenced_key_versions().await,
        Err(RotationGuardError::Unreadable { version, .. }) if version == "ck-absent"
    ));
    assert_fails_closed(&h, &r, &digits, "row names an absent version").await;
    assert_eq!(
        audited(&h, r.contact, "RESOLVE_FOR_EXECUTION", "FAILED").await,
        2
    );
    assert_eq!(
        audited(&h, r.contact, "RESOLVE_FOR_EXECUTION", "SUCCEEDED").await,
        0
    );

    // Control: the referenced version restored, the same grant resolves.
    common::exec(
        h.conn(),
        &format!(
            "UPDATE protected_contacts SET encryption_key_version = 'ck-live-1' WHERE id = '{}'",
            r.contact
        ),
    )
    .await
    .unwrap();
    h.contacts.verify_referenced_key_versions().await.unwrap();
    let ok = resolve(&h, &r).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    assert_eq!(ok.json["resolution"]["e164"], SOURCE_E164);
}
