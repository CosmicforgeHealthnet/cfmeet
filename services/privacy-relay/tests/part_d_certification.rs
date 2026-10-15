//! Part D certification (§§144–155) inside Privacy Relay: the generic protected-contact
//! flow, reassignment, resolution security, relay assignment with inbound mapping,
//! cross-tenant certification, failure injection and key-rotation-through-resolution —
//! each ending with a scan proving no canary number exists outside the vault's
//! ciphertext. (Flow 6 — evidence — and first-party / external parity run across both
//! real services in Calling's `privacy_relay_e2e.rs`; the delayed-callback flow 5 is
//! `relay_inbound.rs`; fingerprint rotation is `contact_equality.rs`.)

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::{ApplicationEnvironmentKind, ServiceId};
use common::{at, iso, rows, scalar, Consumer, Harness};
use privacy_relay::{
    clock::ManualClock,
    commands::IdempotencyKey,
    config::FeatureFlag,
    contact_vault::{
        keyring::{
            activate_key_version, add_key_version, generate_keyring_json, FileKeyring, KeyPurpose,
            KeyVersion,
        },
        ContactActor, ContactVault,
    },
    enforcement::UnconfiguredCallingEnforcer,
    scope::{CommandContext, RelayScope},
    service::RelayService,
};
use serde_json::{json, Value};
use uuid::Uuid;

const CANARY_A: &str = "+44 20 7946 0811";
const CANARY_B: &str = "+44 20 7946 0812";
const CANARY_C: &str = "+44 20 7946 0813";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

/// Every non-vault table, and the vault tables without their random-byte columns.
async fn non_ciphertext_dump(h: &Harness) -> String {
    let tables = rows(
        h.conn(),
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
    )
    .await;
    let mut out = String::new();
    for t in tables {
        let r = rows(
            h.conn(),
            &format!(
                "SELECT (to_jsonb(t) - 'ciphertext' - 'nonce' - 'fingerprint')::text FROM {} t",
                t[0]
            ),
        )
        .await;
        for row in r {
            out.push_str(&row[0]);
            out.push('\n');
        }
    }
    out
}

fn assert_no_canary(dump: &str, canaries: &[&str]) {
    for canary in canaries {
        let digits: String = canary.chars().filter(char::is_ascii_digit).collect();
        for needle in [canary.to_string(), digits.clone(), digits[2..].to_string()] {
            assert!(
                !dump.contains(&needle),
                "{needle} found outside the vault ciphertext"
            );
        }
    }
}

async fn verified(h: &Harness, c: &Consumer, number: &str) -> String {
    let id = h.create_contact(c, number).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(c, &id, "VERIFIED", None).await;
    id
}

async fn authorize(
    h: &Harness,
    c: &Consumer,
    session: &str,
    source: Value,
    target: Value,
) -> Value {
    let r = h
        .post(
            "/internal/v1/communication/authorize",
            c,
            json!({ "relaySessionId": session, "source": source, "targets": [target], "channel": "VOICE", "route": "PUBLIC_RELAY", "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

#[tokio::test]
async fn flow_1_generic_protected_contacts_flow_and_flow_2_reassignment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let principal = Uuid::new_v4();
    // 1. a generic context (principals only), 2.–4. two protected contacts via delegation.
    let colleague = Uuid::new_v4();
    let mut req = common::context_request(&[principal, colleague], "BIDIRECTIONAL", &[]);
    req["allowedRelayStrategies"] = json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let a = verified(&h, &c, CANARY_A).await;
    let b = h.create_contact(&c, CANARY_B).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    // 5. ContactReferences join the context; 6. B is verified through the trusted scope.
    let r = h
        .command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 1, "participants": [
            { "principalId": principal }, { "contactReferenceId": a }, { "contactReferenceId": b }] }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    h.verify_contact(&c, &b, "VERIFIED", None).await;
    // 7. a relay session; 8. communication is authorized for verified protected contacts.
    let s = h
        .create_session(&c, &id, json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }))
        .await;
    let sid = s["relaySessionId"].as_str().unwrap().to_string();
    let p = json!({ "principalId": principal });
    let (ca, cb) = (
        json!({ "contactReferenceId": a }),
        json!({ "contactReferenceId": b }),
    );
    assert_eq!(
        authorize(&h, &c, &sid, p.clone(), cb.clone()).await["decision"],
        "AUTHORIZED"
    );
    assert_eq!(
        authorize(&h, &c, &sid, ca.clone(), cb.clone()).await["decision"],
        "AUTHORIZED"
    );

    // Flow 2: replace B with C. Immediately A -> B is denied and A -> C allowed.
    let c3 = verified(&h, &c, CANARY_C).await;
    let r = h
        .command(&format!("/internal/v1/contexts/{id}/update"), &c, json!({ "expectedVersion": 2, "participants": [
            { "principalId": principal }, { "contactReferenceId": a }, { "contactReferenceId": c3 }] }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        authorize(&h, &c, &sid, ca.clone(), cb).await["reason"],
        "PARTICIPANT_NOT_AUTHORIZED"
    );
    assert_eq!(
        authorize(&h, &c, &sid, ca, json!({ "contactReferenceId": c3 })).await["decision"],
        "AUTHORIZED"
    );
    // B's plaintext stays protected: masked only, nothing outside the ciphertext.
    let viewed = h
        .contact_call(
            Method::GET,
            &format!("/v1/privacy-relay/contacts/{b}"),
            Some(&h.writer(&c)),
            None,
        )
        .await;
    assert_eq!(
        viewed.json["contact"]["maskedDisplay"],
        "+44\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}0812"
    );
    // 9. no raw number outside the Contact Vault ciphertext.
    assert_no_canary(
        &non_ciphertext_dump(&h).await,
        &[CANARY_A, CANARY_B, CANARY_C],
    );
}

#[tokio::test]
async fn flow_3_resolution_security_and_flow_4_relay_assignment() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let principal = Uuid::new_v4();
    let contact = verified(&h, &c, CANARY_A).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h
        .create_session(&c, &id, json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }))
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    // Flow 3: Calling holds a protected authorization and asks for a grant.
    let decision = authorize(
        &h,
        &c,
        &sid,
        json!({ "principalId": principal }),
        json!({ "contactReferenceId": contact }),
    )
    .await;
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = h
        .post(&format!("/internal/v1/contacts/{contact}/resolution-grants"), &c, json!({
            "relaySessionId": sid, "decisionId": decision["decisionId"], "callId": call, "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }))
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    let grant = g.json["grant"].clone();
    let body = json!({ "grantId": grant["grantId"], "grantSecret": grant["grantSecret"], "callId": call, "callLegId": leg, "executionAttemptId": Uuid::new_v4() });
    let path = format!("/internal/v1/contacts/{contact}/resolve-for-execution");
    let calling = h.ids.calling.clone();
    let denied = h
        .send_as(&calling, Method::POST, &path, Some(&c), Some(body.clone()))
        .await;
    assert_eq!(denied.code(), "INTERNAL_PERMISSION_DENIED");
    let telephony = h.ids.telephony.clone();
    let ok = h
        .send_as(&telephony, Method::POST, &path, Some(&c), Some(body))
        .await;
    assert_eq!(
        ok.json["resolution"]["e164"], "+442079460811",
        "{}",
        ok.text
    );
    let audit = rows(h.conn(), &format!(
        "SELECT actor_service, purpose, call_ref::text, call_leg_ref::text FROM contact_access_audit \
         WHERE contact_reference = '{contact}' AND operation = 'RESOLVE_FOR_EXECUTION' AND outcome = 'SUCCEEDED'")).await;
    assert_eq!(
        audit,
        vec![vec![
            "cfmeet.telephony".to_string(),
            "PUBLIC_NETWORK_CALL_LEG".into(),
            call.to_string(),
            leg.to_string()
        ]]
    );

    // Flow 4: the verified session gets a temporary relay identity; a replay is the same
    // assignment; another session cannot claim the number; revocation ends the mapping.
    h.add_relay_number("+44 20 7946 0500", "pb-cert").await;
    let first = h.assign_relay_number(&c, &sid).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    assert_eq!(
        h.assign_relay_number(&c, &sid).await.json["assignment"]["assignmentId"],
        first.json["assignment"]["assignmentId"]
    );
    let callback = h
        .create_session(
            &c,
            &id,
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        h.assign_relay_number(&c, &callback).await.code(),
        "RELAY_NUMBER_UNAVAILABLE"
    );
    let v = h
        .get(&format!("/internal/v1/sessions/{sid}"), &c)
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    assert_eq!(
        h.command(
            &format!("/internal/v1/sessions/{sid}/revoke"),
            &c,
            json!({ "expectedVersion": v })
        )
        .await
        .status,
        StatusCode::OK
    );
    let inbound = h
        .telephony(
            "/internal/v1/relay-routing/resolve-inbound",
            json!({ "calledRelayIdentity": "+44 20 7946 0500", "providerObservedAt": iso(at(0)) }),
        )
        .await;
    assert_eq!(inbound.code(), "RELAY_MAPPING_NOT_ACTIVE");
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM relay_number_inventory WHERE operational_status = 'COOLDOWN'"
        )
        .await,
        1
    );
    assert_no_canary(&non_ciphertext_dump(&h).await, &[CANARY_A]);
}

#[tokio::test]
async fn tenant_b_cannot_touch_anything_of_tenant_a_and_learns_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = consumer();
    let b = consumer();
    let principal = Uuid::new_v4();
    let contact = verified(&h, &a, CANARY_A).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(&a, req).await;
    let sid = h
        .create_session(
            &a,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let unknown = Uuid::new_v4().to_string();
    let norm = |r: &common::Response| {
        let mut v = r.normalized_error();
        v["error"]["message"] = Value::Null;
        (r.status, v)
    };
    // Contact operations: read, update, verify, disable — B gets exactly "not found".
    let (wb, vb) = (h.writer(&b), h.verifier_token(&b));
    for (method, suffix, token, body) in [
        (Method::GET, "", &wb, None),
        (
            Method::PATCH,
            "",
            &wb,
            Some(json!({ "expectedVersion": 2, "telephoneNumber": CANARY_B })),
        ),
        (
            Method::POST,
            "/verification",
            &vb,
            Some(
                json!({ "expectedVersion": 2, "targetState": "BLOCKED", "trustSource": "FRAUD_REVIEW" }),
            ),
        ),
        (
            Method::POST,
            "/disable",
            &wb,
            Some(json!({ "expectedVersion": 2 })),
        ),
    ] {
        let theirs = h
            .contact_call(
                method.clone(),
                &format!("/v1/privacy-relay/contacts/{contact}{suffix}"),
                Some(token),
                body.clone(),
            )
            .await;
        let none = h
            .contact_call(
                method,
                &format!("/v1/privacy-relay/contacts/{unknown}{suffix}"),
                Some(token),
                body,
            )
            .await;
        assert_eq!(
            theirs.code(),
            "CONTACT_NOT_FOUND",
            "{suffix}: {}",
            theirs.text
        );
        assert_eq!(norm(&theirs), norm(&none), "{suffix}");
    }
    // Same number in B's scope: an unrelated, new contact (no existence signal).
    let created = h
        .contact_call(
            Method::POST,
            "/v1/privacy-relay/contacts",
            Some(&wb),
            Some(json!({ "telephoneNumber": CANARY_A })),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_ne!(created.json["contact"]["contactReference"], json!(contact));
    // Resolution grants for A's contact / session from B's scope.
    let g = h
        .post(&format!("/internal/v1/contacts/{contact}/resolution-grants"), &b, json!({
            "relaySessionId": sid, "decisionId": Uuid::new_v4(), "callId": Uuid::new_v4(), "callLegId": Uuid::new_v4(), "purpose": "PUBLIC_NETWORK_CALL_LEG" }))
        .await;
    assert!(
        g.status.is_client_error() && g.json.get("grant").is_none(),
        "{}",
        g.text
    );
    // ... and the refusal is byte-identical (status + body) to the one for a random
    // unknown ContactReference: no existence oracle.
    let g_unknown = h
        .post(&format!("/internal/v1/contacts/{}/resolution-grants", Uuid::new_v4()), &b, json!({
            "relaySessionId": sid, "decisionId": Uuid::new_v4(), "callId": Uuid::new_v4(), "callLegId": Uuid::new_v4(), "purpose": "PUBLIC_NETWORK_CALL_LEG" }))
        .await;
    assert_eq!(g.status, g_unknown.status);
    assert_eq!(g.normalized_error(), g_unknown.normalized_error());
    assert_eq!(g.code(), "CONTACT_NOT_FOUND");
    assert_eq!(
        g.text
            .replace(g.json["error"]["requestId"].as_str().unwrap_or(""), "req"),
        g_unknown.text.replace(
            g_unknown.json["error"]["requestId"].as_str().unwrap_or(""),
            "req"
        ),
        "byte-identical apart from the request id"
    );
    // Relay resources: A's session from B is SESSION_NOT_FOUND like an unknown one.
    h.add_relay_number("+44 20 7946 0500", "pb-cert").await;
    let theirs = h.assign_relay_number(&b, &sid).await;
    let none = h.assign_relay_number(&b, &unknown).await;
    assert_eq!(theirs.code(), "SESSION_NOT_FOUND");
    assert_eq!(norm(&theirs), norm(&none));
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM relay_number_assignments").await,
        0
    );
    // Nothing of A was changed.
    let still = h
        .contact_call(
            Method::GET,
            &format!("/v1/privacy-relay/contacts/{contact}"),
            Some(&h.writer(&a)),
            None,
        )
        .await;
    assert_eq!(
        (
            still.json["contact"]["version"].as_i64(),
            still.json["contact"]["verificationState"].as_str()
        ),
        (Some(2), Some("VERIFIED"))
    );
}

#[tokio::test]
async fn failures_never_produce_an_authoritative_mutation_or_a_plaintext_fallback() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    // Key material unavailable: every write / resolution fails closed.
    let keyless = h.vault_with(None);
    let actor = ContactActor::service(
        RelayScope::of_delegated(&c.delegated()),
        ServiceId::calling(),
        None,
        cf_service_auth::RequestId::generate(),
        cf_service_auth::CorrelationId::generate(),
    );
    let request: privacy_relay::contact_vault::service::CreateContactRequest =
        serde_json::from_value(json!({ "telephoneNumber": CANARY_A })).unwrap();
    let err = keyless.create(&actor, request).await.unwrap_err();
    assert_eq!(err.code().as_str(), "CONTACT_VAULT_DISABLED");
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        0
    );

    // PostgreSQL unavailable: no mutation is acknowledged, nothing is half-written.
    let mut options =
        sea_orm::ConnectOptions::new("postgres://postgres:postgres@127.0.0.1:1/none".to_string());
    options
        .connect_lazy(true)
        .connect_timeout(std::time::Duration::from_millis(300))
        .acquire_timeout(std::time::Duration::from_millis(300))
        .sqlx_logging(false);
    let dead = sea_orm::Database::connect(options).await.unwrap();
    let clock = Arc::new(ManualClock::new(common::t0()));
    let relay = RelayService::new(
        dead.clone(),
        clock.clone(),
        FeatureFlag::new(true),
        cf_service_auth::RuntimeEnvironment::Test,
        Arc::new(UnconfiguredCallingEnforcer),
    );
    let cmd = CommandContext::for_delegated(&c.delegated(), ServiceId::calling());
    let r = relay
        .create_context(
            &cmd,
            IdempotencyKey::parse("cert-dead-db").unwrap(),
            serde_json::from_value(common::context_request(
                &[Uuid::new_v4()],
                "BIDIRECTIONAL",
                &[],
            ))
            .unwrap(),
        )
        .await;
    assert_eq!(r.unwrap_err().code().as_str(), "SERVICE_UNAVAILABLE");
    let vault = ContactVault::new(
        dead,
        clock,
        FeatureFlag::new(true),
        Some(h.vault_keys.clone()),
    );
    let request: privacy_relay::contact_vault::service::CreateContactRequest =
        serde_json::from_value(json!({ "telephoneNumber": CANARY_A })).unwrap();
    assert_eq!(
        vault
            .create(&actor, request)
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "SERVICE_UNAVAILABLE"
    );

    // A relay worker that "crashed" (never ran): an expired assignment still cannot route
    // (read-time checks), and the next worker instance recovers the state exactly once.
    let principal = Uuid::new_v4();
    let contact = verified(&h, &c, CANARY_B).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["CALLBACK_RELAY_ALLOWED"]);
    req["expiresAt"] = json!(iso(at(20)));
    let ctx = h.create_context(&c, req).await;
    let sid = h
        .create_session(
            &c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    h.add_relay_number("+44 20 7946 0500", "pb-cert").await;
    assert_eq!(
        h.assign_relay_number(&c, &sid).await.status,
        StatusCode::CREATED
    );
    h.clock.set(at(25));
    let r = h
        .telephony(
            "/internal/v1/relay-routing/resolve-inbound",
            json!({ "calledRelayIdentity": "+44 20 7946 0500", "providerObservedAt": iso(at(25)) }),
        )
        .await;
    assert_eq!(
        r.code(),
        "RELAY_MAPPING_NOT_ACTIVE",
        "no worker ran, still no routing"
    );
    let fresh = h.sibling();
    let report = fresh.run_sweep().await.unwrap();
    assert_eq!(report.contexts_expired, 1);
    assert_eq!(scalar(h.conn(), "SELECT count(*) FROM relay_number_assignments WHERE status = 'COOLDOWN' AND end_reason = 'SESSION_EXPIRED'").await, 1);
    // Long after the cooldown, a late worker returns the number exactly once.
    h.clock.set(at(500));
    let other = h.sibling();
    let (x, y) = tokio::join!(fresh.run_sweep(), other.run_sweep());
    assert_eq!(
        x.unwrap().relay_cooldowns_completed + y.unwrap().relay_cooldowns_completed,
        1
    );
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM relay_number_inventory WHERE operational_status = 'AVAILABLE'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn key_rotation_keeps_references_metadata_and_resolution_intact() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let v1 = KeyVersion::parse("ck-cert-1").unwrap();
    let v2 = KeyVersion::parse("ck-cert-2").unwrap();
    let doc1 = generate_keyring_json(KeyPurpose::Encryption, &v1);
    let base = common::test_vault_keys("unused-e", "fk-cert-1");
    let keys = |doc: &str| {
        base.with_encryption(Arc::new(
            FileKeyring::from_json(doc, KeyPurpose::Encryption).unwrap(),
        ))
        .unwrap()
    };
    h.set_vault_keys(keys(&doc1));
    let c = consumer();
    let principal = Uuid::new_v4();
    let contact = verified(&h, &c, CANARY_A).await;
    let before = h
        .contact_call(
            Method::GET,
            &format!("/v1/privacy-relay/contacts/{contact}"),
            Some(&h.writer(&c)),
            None,
        )
        .await
        .json;
    // Add V2, switch writes to V2, new contacts are V2, re-encrypt V1, retire V1.
    let doc2 = activate_key_version(
        &add_key_version(&doc1, KeyPurpose::Encryption, &v2).unwrap(),
        KeyPurpose::Encryption,
        &v2,
    )
    .unwrap();
    h.set_vault_keys(keys(&doc2));
    let newer = h.create_contact(&c, CANARY_B).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        rows(
            h.conn(),
            &format!("SELECT encryption_key_version FROM protected_contacts WHERE id = '{newer}'")
        )
        .await[0][0],
        "ck-cert-2"
    );
    while h.contacts.rewrap_batch(10).await.unwrap().rewrapped > 0 {}
    let mut only_v2: Value = serde_json::from_str(&doc2).unwrap();
    only_v2["keys"]
        .as_array_mut()
        .unwrap()
        .retain(|k| k["version"] == "ck-cert-2");
    h.set_vault_keys(keys(&only_v2.to_string()));
    assert_eq!(
        h.contacts.key_version_counts().await.unwrap(),
        vec![("ck-cert-2".into(), 2)]
    );
    // Reference and safe metadata unchanged (the version moved only with the rewrap).
    let after = h
        .contact_call(
            Method::GET,
            &format!("/v1/privacy-relay/contacts/{contact}"),
            Some(&h.writer(&c)),
            None,
        )
        .await
        .json;
    for k in [
        "contactReference",
        "maskedDisplay",
        "country",
        "verificationState",
        "version",
    ] {
        assert_eq!(after["contact"][k], before["contact"][k], "{k}");
    }
    // Resolution still returns the correct canonical value — with V1 gone.
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(&c, req).await;
    let sid = h
        .create_session(
            &c,
            ctx["contextId"].as_str().unwrap(),
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let decision = authorize(
        &h,
        &c,
        &sid,
        json!({ "principalId": principal }),
        json!({ "contactReferenceId": contact }),
    )
    .await;
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = h
        .post(&format!("/internal/v1/contacts/{contact}/resolution-grants"), &c, json!({
            "relaySessionId": sid, "decisionId": decision["decisionId"], "callId": call, "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }))
        .await
        .json["grant"]
        .clone();
    let telephony = h.ids.telephony.clone();
    let r = h
        .send_as(&telephony, Method::POST, &format!("/internal/v1/contacts/{contact}/resolve-for-execution"), Some(&c), Some(json!({
            "grantId": g["grantId"], "grantSecret": g["grantSecret"], "callId": call, "callLegId": leg, "executionAttemptId": Uuid::new_v4() })))
        .await;
    assert_eq!(r.json["resolution"]["e164"], "+442079460811", "{}", r.text);
    assert_no_canary(&non_ciphertext_dump(&h).await, &[CANARY_A, CANARY_B]);
}
