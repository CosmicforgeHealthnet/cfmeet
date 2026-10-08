//! Part D §32: ContactReference equality — keyed, scope-bound fingerprints; exact
//! deduplication inside one application environment; no global equality identity, no
//! cross-tenant existence signal; fingerprint-key rotation without a dedup outage.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind;
use common::{dump_database, rows, scalar, test_vault_keys, Consumer, Harness};
use privacy_relay::contact_vault::keyring::{
    activate_key_version, add_key_version, generate_keyring_json, FileKeyring, KeyPurpose,
    KeyVersion,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const NUMBER: &str = "+39 02 1234 5678";
const E164: &str = "+390212345678";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

async fn create(h: &Harness, c: &Consumer, number: &str) -> (StatusCode, String) {
    let r = h
        .contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&h.writer(c)),
            Some(json!({ "telephoneNumber": number })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    (
        r.status,
        r.json["contact"]["contactReference"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

async fn fingerprints_of(h: &Harness, contact: &str) -> Vec<Vec<String>> {
    rows(
        h.conn(),
        &format!("SELECT fingerprint_key_version, encode(fingerprint, 'hex') FROM contact_fingerprints WHERE contact_reference = '{contact}' ORDER BY 1"),
    )
    .await
}

#[tokio::test]
async fn the_same_number_in_one_scope_is_one_contact_and_independent_across_scopes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = consumer();
    let (status, first) = create(&h, &a, NUMBER).await;
    assert_eq!(status, StatusCode::CREATED);
    // Same number, other formatting, same scope: the existing ContactReference (200).
    for variant in [
        "+390212345678",
        "+39 (02) 1234-5678",
        " +39 02 12 34 56 78 ",
    ] {
        let (status, again) = create(&h, &a, variant).await;
        assert_eq!(
            (status, again.as_str()),
            (StatusCode::OK, first.as_str()),
            "{variant}"
        );
    }
    let national = h
        .contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&h.writer(&a)),
            Some(json!({ "telephoneNumber": "02 1234 5678", "defaultCountry": "IT" })),
        )
        .await;
    assert_eq!(national.json["contact"]["contactReference"], first.as_str());
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        1
    );
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM contact_access_audit WHERE contact_reference = '{first}' AND operation = 'CREATE' AND reason_code = 'DEDUPLICATED'")).await,
        4
    );
    // Another tenant, another application, another environment — each differing ALONE
    // (every scope component is inside the MAC): independent contacts and unrelated
    // fingerprints — and the same 201 a never-seen number gets.
    let mut tenant_b = a.clone();
    tenant_b.tenant_id = Uuid::new_v4();
    let mut app_b = a.clone();
    app_b.application_id = Uuid::new_v4();
    let env_b = a.sibling_environment();
    let mut refs = vec![first.clone()];
    let mut fps = vec![fingerprints_of(&h, &first).await];
    for other in [&tenant_b, &app_b, &env_b] {
        let (status, id) = create(&h, other, NUMBER).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "no existence signal across scopes"
        );
        assert!(!refs.contains(&id));
        let fp = fingerprints_of(&h, &id).await;
        assert!(
            !fps.iter().any(|f| f[0][1] == fp[0][1]),
            "fingerprints are scope-bound"
        );
        refs.push(id);
        fps.push(fp);
    }
    // Nothing about the other scopes was recorded in A's scope.
    assert_eq!(
        scalar(h.conn(), &format!(
            "SELECT count(*) FROM contact_access_audit WHERE tenant_id = '{}' AND application_id = '{}' AND environment_id = '{}' AND contact_reference <> '{first}'",
            a.tenant_id, a.application_id, a.environment_id
        )).await,
        0
    );
}

#[tokio::test]
async fn no_plaintext_or_unkeyed_hash_exists_and_a_fingerprint_is_not_reversible_from_the_database()
{
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = consumer();
    let (_, id) = create(&h, &a, NUMBER).await;
    let dump = dump_database(h.conn()).await;
    let plain = hex::encode(Sha256::digest(E164.as_bytes()));
    let scoped_plain = hex::encode(Sha256::digest(
        format!(
            "cfmeet-contact-v1\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{E164}",
            a.tenant_id, a.application_id, a.environment_id
        )
        .as_bytes(),
    ));
    for needle in [E164, "0212345678", plain.as_str(), scoped_plain.as_str()] {
        assert!(!dump.contains(needle), "{needle} is in the database");
    }
    // An attacker holding the database (and the scope ids) but not the fingerprint key
    // cannot confirm a guess: neither an unkeyed hash nor an HMAC under any other key of
    // the correct guess equals the stored fingerprint.
    let stored = &fingerprints_of(&h, &id).await[0][1];
    for guess in ["+390212345678", "+390212345679", "+390212345677"] {
        assert_ne!(&hex::encode(Sha256::digest(guess.as_bytes())), stored);
    }
    let wrong_key = test_vault_keys("x-e", "x-f");
    let (wv, _) = wrong_key.fingerprint().current_key();
    let n = privacy_relay::contact_vault::normalization::normalize(
        &privacy_relay::contact_vault::normalization::RawContactValue::new(E164),
        None,
    )
    .unwrap();
    let scope = privacy_relay::scope::RelayScope::of_delegated(&a.delegated());
    let other =
        privacy_relay::contact_vault::fingerprint::under(wrong_key.fingerprint(), wv, scope, &n)
            .unwrap();
    assert_ne!(&hex::encode(other.value), stored);
    // ...while the vault's own key reproduces it.
    let (v, _) = h.vault_keys.fingerprint().current_key();
    let ours =
        privacy_relay::contact_vault::fingerprint::under(h.vault_keys.fingerprint(), v, scope, &n)
            .unwrap();
    assert_eq!(&hex::encode(ours.value), stored);
    // No unique or plain index over a number exists.
    let indexes = dump_indexes(&h).await;
    assert!(!indexes.contains("ciphertext"), "{indexes}");
}

async fn dump_indexes(h: &Harness) -> String {
    rows(h.conn(), "SELECT indexdef FROM pg_indexes WHERE schemaname = 'public' AND tablename IN ('protected_contacts', 'contact_fingerprints')")
        .await
        .concat()
        .join("\n")
}

#[tokio::test]
async fn concurrent_creates_of_one_number_yield_exactly_one_contact() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = consumer();
    let token = h.writer(&a);
    let results = futures::future::join_all((0..8).map(|i| {
        let token = token.clone();
        let h = &h;
        async move {
            let number = if i % 2 == 0 { NUMBER } else { "+390212345678" };
            h.contact_call(
                Method::POST,
                "/v1/privacy-relay/contacts",
                Some(&token),
                Some(json!({ "telephoneNumber": number })),
            )
            .await
        }
    }))
    .await;
    let ids: std::collections::BTreeSet<String> = results
        .iter()
        .map(|r| {
            assert!(r.status.is_success(), "{}", r.text);
            r.json["contact"]["contactReference"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r.status == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        1
    );
}

#[tokio::test]
async fn updates_respect_equality_and_disabling_releases_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (consumer(), consumer());
    let (_, first) = create(&h, &a, NUMBER).await;
    let (_, second) = create(&h, &a, "+44 20 7946 0958").await;
    let (_, foreign) = create(&h, &b, "+234 803 123 4567").await;
    let token = h.writer(&a);
    // Changing `second` to the number `first` protects: 409 within the scope.
    let r = h
        .contact_call(
            Method::PATCH,
            &format!("/v1/privacy-relay/contacts/{second}"),
            Some(&token),
            Some(json!({ "expectedVersion": 1, "telephoneNumber": "+390212345678" })),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "CONTACT_ALREADY_EXISTS")
    );
    // A number another TENANT protects is irrelevant here.
    let r = h
        .contact_call(
            Method::PATCH,
            &format!("/v1/privacy-relay/contacts/{second}"),
            Some(&token),
            Some(json!({ "expectedVersion": 1, "telephoneNumber": "+234 803 123 4567" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // The old value's equality is released: protecting it again is a new contact.
    let (status, again) = create(&h, &a, "+44 20 7946 0958").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(again, second);
    assert_eq!(
        fingerprints_of(&h, &second).await.len(),
        1,
        "one fingerprint per key version"
    );
    let _ = foreign;
    // Disabling releases equality too.
    h.contact_call(
        Method::POST,
        &format!("/v1/privacy-relay/contacts/{first}/disable"),
        Some(&token),
        Some(json!({ "expectedVersion": 1 })),
    )
    .await;
    assert!(fingerprints_of(&h, &first).await.is_empty());
    let (status, renewed) = create(&h, &a, NUMBER).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_ne!(renewed, first);
    // The database refuses a duplicate fingerprint in a scope, and any rewrite of one.
    let copy = format!(
        "INSERT INTO contact_fingerprints SELECT '{second}', tenant_id, application_id, environment_id, 'other-version', fingerprint, now() \
         FROM contact_fingerprints WHERE contact_reference = '{renewed}'"
    );
    common::exec(h.conn(), &copy).await.unwrap();
    let dup = format!(
        "INSERT INTO contact_fingerprints SELECT '{again}', tenant_id, application_id, environment_id, fingerprint_key_version, fingerprint, now() \
         FROM contact_fingerprints WHERE contact_reference = '{renewed}'"
    );
    assert!(common::exec(h.conn(), &dup).await.is_err());
    assert!(common::exec(
        h.conn(),
        "UPDATE contact_fingerprints SET fingerprint = fingerprint"
    )
    .await
    .is_err());
}

#[tokio::test]
async fn contexts_reference_only_enabled_contacts_of_their_own_scope() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (consumer(), consumer());
    let principal = Uuid::new_v4();
    let (_, mine) = create(&h, &a, NUMBER).await;
    let (_, theirs) = create(&h, &b, NUMBER).await;
    let request = |contact: &str| {
        let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
        req["participants"] =
            json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
        req
    };
    let ok = h.command("/internal/v1/contexts", &a, request(&mine)).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
    let foreign = h
        .command("/internal/v1/contexts", &a, request(&theirs))
        .await;
    let unknown = h
        .command(
            "/internal/v1/contexts",
            &a,
            request(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(
        (foreign.status, foreign.code()),
        (StatusCode::NOT_FOUND, "CONTACT_NOT_FOUND")
    );
    assert_eq!(foreign.normalized_error(), unknown.normalized_error());
    // A disabled contact cannot be newly added (an existing participant stays referenced
    // but is never resolved).
    let ctx_id = ok.json["context"]["contextId"].as_str().unwrap();
    let (_, disabled) = create(&h, &a, "+44 20 7946 0958").await;
    h.contact_call(
        Method::POST,
        &format!("/v1/privacy-relay/contacts/{disabled}/disable"),
        Some(&h.writer(&a)),
        Some(json!({ "expectedVersion": 1 })),
    )
    .await;
    let r = h
        .command(&format!("/internal/v1/contexts/{ctx_id}/update"), &a, json!({ "expectedVersion": 1, "participants": [{ "principalId": principal }, { "contactReferenceId": mine }, { "contactReferenceId": disabled }] }))
        .await;
    assert_eq!(r.code(), "CONTACT_NOT_FOUND", "{}", r.text);
    let (_, fresh) = create(&h, &a, "+234 803 123 4567").await;
    let r = h
        .command(&format!("/internal/v1/contexts/{ctx_id}/update"), &a, json!({ "expectedVersion": 1, "participants": [{ "principalId": principal }, { "contactReferenceId": mine }, { "contactReferenceId": fresh }] }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn fingerprint_key_rotation_keeps_deduplication_working() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f1 = KeyVersion::parse("fk-v1").unwrap();
    let f2 = KeyVersion::parse("fk-v2").unwrap();
    let doc1 = generate_keyring_json(KeyPurpose::Fingerprint, &f1);
    let with = |h: &Harness, doc: &str| {
        h.vault_keys
            .with_fingerprint(Arc::new(
                FileKeyring::from_json(doc, KeyPurpose::Fingerprint).unwrap(),
            ))
            .unwrap()
    };
    let keys = with(&h, &doc1);
    h.set_vault_keys(keys);
    let a = consumer();
    let numbers = [NUMBER, "+44 20 7946 0958", "+234 803 123 4567"];
    let mut ids = Vec::new();
    for n in numbers {
        ids.push(create(&h, &a, n).await.1);
    }
    // 1. Load the NEW key (readable; V1 stays active).
    let doc_overlap = add_key_version(&doc1, KeyPurpose::Fingerprint, &f2).unwrap();
    let keys = with(&h, &doc_overlap);
    h.set_vault_keys(keys);
    // Matching during the overlap (old fingerprints only so far).
    assert_eq!(
        create(&h, &a, NUMBER).await,
        (StatusCode::OK, ids[0].clone())
    );
    // 2. Backfill the new version (bounded, resumable, concurrent-safe).
    let other = h.vault_with(Some(h.vault_keys.clone()));
    let (x, y) = tokio::join!(
        h.contacts.backfill_fingerprints(1),
        other.backfill_fingerprints(1)
    );
    assert_eq!(x.unwrap().backfilled + y.unwrap().backfilled, 2);
    let rest = h.contacts.backfill_fingerprints(10).await.unwrap();
    assert_eq!((rest.backfilled, rest.remaining), (1, 0));
    assert_eq!(
        h.contacts.fingerprint_version_counts().await.unwrap(),
        vec![("fk-v1".into(), 3), ("fk-v2".into(), 3)]
    );
    // 3. Make NEW active.
    let doc2 = activate_key_version(&doc_overlap, KeyPurpose::Fingerprint, &f2).unwrap();
    let keys = with(&h, &doc2);
    h.set_vault_keys(keys);
    for (n, id) in numbers.iter().zip(&ids) {
        assert_eq!(create(&h, &a, n).await, (StatusCode::OK, id.clone()), "{n}");
    }
    let (status, newer) = create(&h, &a, "+1 650 253 0000").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        fingerprints_of(&h, &newer).await.len(),
        2,
        "new contacts carry every readable version"
    );
    // 4. Prune OLD only when nothing would lose equality.
    assert!(
        h.contacts.prune_fingerprint_version("fk-v2").await.is_err(),
        "the active version is never pruned"
    );
    common::exec(h.conn(), &format!("DELETE FROM contact_fingerprints WHERE contact_reference = '{}' AND fingerprint_key_version = 'fk-v2'", ids[1])).await.unwrap();
    assert!(
        h.contacts.prune_fingerprint_version("fk-v1").await.is_err(),
        "a gap blocks the prune"
    );
    h.contacts.run_sweep().await.unwrap();
    assert_eq!(
        h.contacts.prune_fingerprint_version("fk-v1").await.unwrap(),
        4
    );
    assert_eq!(
        h.contacts.fingerprint_version_counts().await.unwrap(),
        vec![("fk-v2".into(), 4)]
    );
    // 5. The old key can now be retired: a V2-only keyring still deduplicates everything.
    let mut only: serde_json::Value = serde_json::from_str(&doc2).unwrap();
    only["keys"]
        .as_array_mut()
        .unwrap()
        .retain(|k| k["version"] == "fk-v2");
    let keys = with(&h, &only.to_string());
    h.set_vault_keys(keys);
    for (n, id) in numbers.iter().zip(&ids) {
        assert_eq!(create(&h, &a, n).await, (StatusCode::OK, id.clone()), "{n}");
    }
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        4,
        "no duplicate during the transition"
    );
}
