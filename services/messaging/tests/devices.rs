//! Part V C.4 / C.13: device registration with proof of possession, KeyPackages,
//! revocation, the device directory — with real OpenMLS client devices.

mod common;

use axum::http::{Method, StatusCode};
use cf_messaging_crypto::device::Device;
use common::*;
use messaging::identity::PrincipalStatus;
use serde_json::json;
use uuid::Uuid;

const SEND: &str = "messages.send";
const READ: &str = "messages.read";

#[tokio::test]
async fn a_device_registers_with_a_signed_challenge_and_stores_public_material_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&[SEND, READ]);
    let alice = h.principal(&c);
    let ch = h.challenge(&c, alice).await;
    assert_eq!(ch.status, StatusCode::CREATED, "{}", ch.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/RegistrationChallenge",
        &ch.json,
    );
    let (device, r) = {
        let device_id = uuid(&ch.json["deviceId"]);
        let challenge_id = uuid(&ch.json["challengeId"]);
        let nonce: [u8; 32] = hex::decode(ch.json["nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let device = Device::generate(device_id).unwrap();
        let kp = device
            .registration_key_package(challenge_id, &nonce)
            .unwrap();
        let body = json!({ "challengeId": challenge_id, "keyPackage": b64(&kp) });
        let r = h
            .post(&c, Some(alice), "/v1/messaging/devices", body.clone())
            .await;
        // Replaying the same answer returns the same device (idempotent).
        let again = h.post(&c, Some(alice), "/v1/messaging/devices", body).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.text);
        assert_eq!(again.json["deviceId"], r.json["deviceId"]);
        (device, r)
    };
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms("messaging/common/v1.schema.json#/$defs/Device", &r.json);
    assert_eq!(r.json["status"], "ACTIVE");
    assert_eq!(r.json["principalId"], alice.to_string());
    assert_eq!(
        unb64(&r.json["signaturePublicKey"]),
        device.signature_public_key()
    );
    assert_eq!(r.json["transparency"]["registrationLeafIndex"], 0);
    // One device, one transparency leaf, one audit row, one outbox event; challenge used.
    assert_eq!(h.scalar("SELECT count(*) FROM messaging_devices").await, 1);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_transparency_leaves")
            .await,
        1
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_audit WHERE action = 'DEVICE_REGISTERED'")
            .await,
        1
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_outbox_events WHERE event_type = 'messaging.device.registered'").await,
        1
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_device_challenges WHERE status = 'CONSUMED'")
            .await,
        1
    );
    // Every event payload conforms to the event contract.
    let payload: serde_json::Value = h
        .conn()
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT payload FROM messaging_outbox_events".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    conforms(
        "messaging/events/v1.schema.json",
        &json!({ "event_type": "messaging.device.registered", "payload": payload }),
    );
}

use sea_orm::ConnectionTrait;

#[tokio::test]
async fn registration_answers_must_match_their_challenge_exactly() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let ch = h.challenge(&c, alice).await;
    let device_id = uuid(&ch.json["deviceId"]);
    let challenge_id = uuid(&ch.json["challengeId"]);
    let nonce: [u8; 32] = hex::decode(ch.json["nonce"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let register = |kp: Vec<u8>| json!({ "challengeId": challenge_id, "keyPackage": b64(&kp) });
    // A device naming another (self-chosen) device id.
    let impostor = Device::generate(Uuid::new_v4()).unwrap();
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/devices",
            register(
                impostor
                    .registration_key_package(challenge_id, &nonce)
                    .unwrap(),
            ),
        )
        .await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::CONFLICT,
            "CHALLENGE_INVALID",
            "BINDING_MISMATCH"
        )
    );
    let device = Device::generate(device_id).unwrap();
    // Wrong nonce (a replayed / guessed binding), and no binding at all.
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/devices",
            register(
                device
                    .registration_key_package(challenge_id, &[0u8; 32])
                    .unwrap(),
            ),
        )
        .await;
    assert_eq!(r.reason(), "BINDING_MISMATCH");
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/devices",
            register(device.key_package().unwrap()),
        )
        .await;
    assert_eq!(r.reason(), "BINDING_MISMATCH");
    // Another principal or another credential of the same environment cannot use it.
    let bob = h.principal(&c);
    let good = device
        .registration_key_package(challenge_id, &nonce)
        .unwrap();
    let r = h
        .post(
            &c,
            Some(bob),
            "/v1/messaging/devices",
            register(good.clone()),
        )
        .await;
    assert_eq!(r.reason(), "BOUND_TO_ANOTHER_REQUEST");
    let other = h.credential(&c, &[SEND]);
    let r = h
        .post(
            &other,
            Some(alice),
            "/v1/messaging/devices",
            register(good.clone()),
        )
        .await;
    assert_eq!(r.reason(), "BOUND_TO_ANOTHER_REQUEST");
    // Another tenant does not even see the challenge.
    let mallory = h.customer(&[SEND]);
    h.set_principal(
        &mallory,
        alice,
        PrincipalStatus::Active {
            kind: "NATIVE_CF_USER".into(),
        },
    );
    let r = h
        .post(
            &mallory,
            Some(alice),
            "/v1/messaging/devices",
            register(good.clone()),
        )
        .await;
    assert_eq!(r.reason(), "UNKNOWN");
    // Garbage and tampered KeyPackages are rejected structurally.
    let r = h
        .post(&c, Some(alice), "/v1/messaging/devices", json!({ "challengeId": challenge_id, "keyPackage": b64(b"{\"publicKey\":\"trust me\"}") }))
        .await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "KEY_PACKAGE_REJECTED",
            "KEY_PACKAGE_MALFORMED"
        )
    );
    let mut tampered = good.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let r = h
        .post(&c, Some(alice), "/v1/messaging/devices", register(tampered))
        .await;
    assert_eq!(r.code(), "KEY_PACKAGE_REJECTED");
    assert_eq!(h.scalar("SELECT count(*) FROM messaging_devices").await, 0);
    // The genuine answer still works, exactly once; a different device key later is refused.
    let r = h
        .post(&c, Some(alice), "/v1/messaging/devices", register(good))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let other_key = Device::generate(device_id).unwrap();
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/devices",
            register(
                other_key
                    .registration_key_package(challenge_id, &nonce)
                    .unwrap(),
            ),
        )
        .await;
    assert_eq!(r.reason(), "CONSUMED");
}

#[tokio::test]
async fn challenges_expire_and_are_rate_limited_per_principal() {
    let Some(h) = Harness::build(|s| {
        s.devices.challenges_per_hour = 2;
    })
    .await
    else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let ch = h.challenge(&c, alice).await;
    h.challenge(&c, alice).await;
    let third = h.challenge(&c, alice).await;
    assert_eq!(third.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(third.headers.get("retry-after").is_some());
    // Another principal has its own window.
    assert_eq!(
        h.challenge(&c, h.principal(&c)).await.status,
        StatusCode::CREATED
    );
    // Expiry.
    h.clock.advance(chrono::Duration::seconds(301));
    let device = Device::generate(uuid(&ch.json["deviceId"])).unwrap();
    let nonce: [u8; 32] = hex::decode(ch.json["nonce"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let r = h
        .post(
            &c,
            Some(alice),
            "/v1/messaging/devices",
            json!({ "challengeId": ch.json["challengeId"], "keyPackage": b64(&device.registration_key_package(uuid(&ch.json["challengeId"]), &nonce).unwrap()) }),
        )
        .await;
    assert_eq!(r.reason(), "EXPIRED");
}

#[tokio::test]
async fn the_acting_principal_must_be_authorized_active_and_messaging_capable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let path = "/v1/messaging/devices/registration-challenges";
    let r = h.post(&c, None, path, json!({})).await;
    assert_eq!(r.code(), "ACTING_PRINCIPAL_REQUIRED");
    // Unknown, inactive, telephone endpoint, another tenant's principal.
    let r = h.post(&c, Some(Uuid::new_v4()), path, json!({})).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::FORBIDDEN, "PRINCIPAL_UNKNOWN")
    );
    let gone = h.principal(&c);
    h.set_principal(&c, gone, PrincipalStatus::Inactive);
    assert_eq!(
        h.post(&c, Some(gone), path, json!({})).await.reason(),
        "PRINCIPAL_INACTIVE"
    );
    let phone = h.principal_of_kind(&c, "TELEPHONE_ENDPOINT");
    assert_eq!(
        h.post(&c, Some(phone), path, json!({})).await.reason(),
        "PRINCIPAL_KIND_NOT_MESSAGING"
    );
    let other = h.customer(&[SEND]);
    let theirs = h.principal(&other);
    assert_eq!(
        h.post(&c, Some(theirs), path, json!({})).await.reason(),
        "PRINCIPAL_UNKNOWN"
    );
    // A credential restricted to bob cannot act for alice (impersonation).
    let bob = h.principal(&c);
    let restricted = h.restricted(&c, &[bob]);
    let r = h.post(&restricted, Some(alice), path, json!({})).await;
    assert_eq!(
        (r.status, r.reason()),
        (StatusCode::FORBIDDEN, "CREDENTIAL_RESTRICTION")
    );
    assert_eq!(
        h.post(&restricted, Some(bob), path, json!({})).await.status,
        StatusCode::CREATED
    );
    // Malformed / duplicated header.
    let r = h
        .send(
            Method::POST,
            Some(&c.token),
            None,
            path,
            Some(json!({})),
            &[("cf-acting-principal", "ALICE")],
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    // Missing scope.
    let reader = h.credential(&c, &[READ]);
    assert_eq!(
        h.post(&reader, Some(alice), path, json!({})).await.code(),
        "INSUFFICIENT_SCOPE"
    );
    // Platform Access unreachable: fail closed.
    *h.principals.down.lock().unwrap() = true;
    assert_eq!(
        h.post(&c, Some(alice), path, json!({})).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn concurrent_registrations_respect_the_active_device_limit() {
    let Some(h) = Harness::build(|s| {
        s.devices.max_active_devices = 2;
    })
    .await
    else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let mut prepared = Vec::new();
    for _ in 0..6 {
        let ch = h.challenge(&c, alice).await;
        let device = Device::generate(uuid(&ch.json["deviceId"])).unwrap();
        let nonce: [u8; 32] = hex::decode(ch.json["nonce"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let kp = device
            .registration_key_package(uuid(&ch.json["challengeId"]), &nonce)
            .unwrap();
        prepared.push(json!({ "challengeId": ch.json["challengeId"], "keyPackage": b64(&kp) }));
    }
    let results = futures_join(prepared.into_iter().map(|body| {
        let h = &h;
        let c = &c;
        async move { h.post(c, Some(alice), "/v1/messaging/devices", body).await }
    }))
    .await;
    let created = results
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    let limited = results
        .iter()
        .filter(|r| r.code() == "DEVICE_LIMIT_REACHED")
        .count();
    assert_eq!((created, limited), (2, 4), "{results:?}");
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_devices WHERE status = 'ACTIVE'")
            .await,
        2
    );
    // Leaves stay gap-free and dense under concurrency.
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_transparency_leaves")
            .await,
        2
    );
    assert_eq!(
        h.scalar("SELECT max(leaf_index) FROM messaging_transparency_leaves")
            .await,
        1
    );
}

async fn futures_join<F: std::future::Future>(futures: impl Iterator<Item = F>) -> Vec<F::Output> {
    let handles: Vec<_> = futures.collect();
    let mut out = Vec::new();
    // Drive them concurrently on the same task.
    let mut pinned: Vec<std::pin::Pin<Box<F>>> = handles.into_iter().map(Box::pin).collect();
    let mut done: Vec<Option<F::Output>> = (0..pinned.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all = true;
        for (i, f) in pinned.iter_mut().enumerate() {
            if done[i].is_none() {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => done[i] = Some(v),
                    std::task::Poll::Pending => all = false,
                }
            }
        }
        if all {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    for d in done {
        out.push(d.unwrap());
    }
    out
}

#[tokio::test]
async fn key_packages_are_bound_to_their_device_unique_and_bounded() {
    let Some(h) = Harness::build(|s| {
        s.devices.max_key_packages_per_device = 3;
    })
    .await
    else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let device = h.active_device(&c, alice).await;
    let id = device.device_id();
    let kp1 = device.key_package().unwrap();
    let kp2 = device.key_package().unwrap();
    let r = h.publish(&c, alice, id, &[kp1.clone(), kp2]).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/PublishKeyPackagesResponse",
        &r.json,
    );
    // Re-submission is idempotent, never a second acceptance (replay prevention).
    let r = h.publish(&c, alice, id, std::slice::from_ref(&kp1)).await;
    assert_eq!(r.json["keyPackages"][0]["outcome"], "ALREADY_PUBLISHED");
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_key_packages")
            .await,
        2
    );
    // Another device's package, a package for the same id with another key, the
    // registration package: all refused.
    let other = h.active_device(&c, alice).await;
    let r = h
        .publish(&c, alice, id, &[other.key_package().unwrap()])
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("KEY_PACKAGE_REJECTED", "DEVICE_MISMATCH")
    );
    let same_id_other_key = Device::generate(id).unwrap();
    let r = h
        .publish(&c, alice, id, &[same_id_other_key.key_package().unwrap()])
        .await;
    assert_eq!(r.reason(), "DEVICE_MISMATCH");
    let r = h
        .publish(
            &c,
            alice,
            id,
            &[device
                .registration_key_package(Uuid::new_v4(), &[1; 32])
                .unwrap()],
        )
        .await;
    assert_eq!(r.reason(), "REGISTRATION_KEY_PACKAGE");
    // Bounded per device: the batch that would exceed the bound is refused atomically.
    let r = h
        .publish(
            &c,
            alice,
            id,
            &[device.key_package().unwrap(), device.key_package().unwrap()],
        )
        .await;
    assert_eq!(r.code(), "KEY_PACKAGE_LIMIT_REACHED");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM messaging_key_packages WHERE device_id = '{id}'"
        ))
        .await,
        2
    );
    // Another principal / tenant cannot publish to this device (NOT_FOUND, not FORBIDDEN).
    let bob = h.principal(&c);
    assert_eq!(
        h.publish(&c, bob, id, &[device.key_package().unwrap()])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let mallory = h.customer(&[SEND]);
    h.set_principal(
        &mallory,
        alice,
        PrincipalStatus::Active {
            kind: "NATIVE_CF_USER".into(),
        },
    );
    assert_eq!(
        h.publish(&mallory, alice, id, &[device.key_package().unwrap()])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Bounds of the request itself.
    assert_eq!(
        h.publish(&c, alice, id, &[]).await.code(),
        "VALIDATION_ERROR"
    );
}

#[tokio::test]
async fn revoked_devices_lose_their_key_packages_and_protected_operations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&[SEND, READ]);
    let alice = h.principal(&c);
    let device = h.active_device(&c, alice).await;
    let id = device.device_id();
    h.publish(
        &c,
        alice,
        id,
        &[device.key_package().unwrap(), device.key_package().unwrap()],
    )
    .await;
    let path = format!("/v1/messaging/devices/{id}/revoke");
    assert_eq!(
        h.post(&c, Some(alice), &path, json!({ "reason": "STOLEN" }))
            .await
            .code(),
        "VALIDATION_ERROR"
    );
    let r = h
        .post(
            &c,
            Some(alice),
            &path,
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "REVOKED");
    assert_eq!(r.json["transparency"]["revocationLeafIndex"], 1);
    conforms("messaging/common/v1.schema.json#/$defs/Device", &r.json);
    // Idempotent.
    assert_eq!(
        h.post(
            &c,
            Some(alice),
            &path,
            json!({ "reason": "LOST_OR_STOLEN" })
        )
        .await
        .json["version"],
        2
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_key_packages WHERE status = 'REVOKED'")
            .await,
        2
    );
    // A revoked device cannot publish, and is gone from the directory.
    let r = h
        .publish(&c, alice, id, &[device.key_package().unwrap()])
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "DEVICE_REVOKED")
    );
    let dir = h
        .get(
            &c,
            Some(alice),
            &format!("/v1/messaging/principals/{alice}/devices"),
        )
        .await;
    assert_eq!(dir.json["devices"], json!([]));
    // REVOKED is terminal in the database itself, and key facts never change.
    assert!(h.sql(&format!("UPDATE messaging_devices SET status = 'ACTIVE', revoked_at = NULL, revocation_reason = NULL, revocation_leaf_index = NULL, version = version + 1 WHERE id = '{id}'")).await.is_err());
    assert!(h
        .sql(&format!("DELETE FROM messaging_devices WHERE id = '{id}'"))
        .await
        .is_err());
    // A signature key identifies one device for ever: the database refuses its re-use.
    assert!(h
        .sql(&format!(
            "INSERT INTO messaging_devices SELECT gen_random_uuid(), tenant_id, application_id, environment_id, principal_id,                'ACTIVE', protocol, ciphersuite, signature_public_key, credential_identity, key_fingerprint, NULL,                registered_by_credential_id, challenge_id, 99, NULL, now(), NULL, NULL, 1 FROM messaging_devices WHERE id = '{id}'"
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn the_device_directory_is_scoped_rate_limited_and_public_only() {
    let Some(h) = Harness::build(|s| {
        s.devices.directory_lookups_per_minute = 5;
    })
    .await
    else {
        return;
    };
    let c = h.customer(&[SEND, READ]);
    let alice = h.principal(&c);
    let bob = h.principal(&c);
    let d1 = h.active_device(&c, alice).await;
    h.active_device(&c, alice).await;
    let r = h
        .get(
            &c,
            Some(bob),
            &format!("/v1/messaging/principals/{alice}/devices"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/DeviceDirectory",
        &r.json,
    );
    assert_eq!(r.json["devices"].as_array().unwrap().len(), 2);
    assert!(r.json["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| unb64(&d["signaturePublicKey"]) == d1.signature_public_key()));
    assert!(!r.text.to_lowercase().contains("private"));
    // Unknown principal and another tenant's principal: NOT_FOUND (no enumeration of
    // other scopes).
    assert_eq!(
        h.get(
            &c,
            Some(bob),
            &format!("/v1/messaging/principals/{}/devices", Uuid::new_v4())
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let other = h.customer(&[READ]);
    let eve = h.principal(&other);
    assert_eq!(
        h.get(
            &other,
            Some(eve),
            &format!("/v1/messaging/principals/{alice}/devices")
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Unauthenticated / unscoped.
    let r = h
        .send(
            Method::GET,
            None,
            Some(bob),
            &format!("/v1/messaging/principals/{alice}/devices"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let sender = h.credential(&c, &[SEND]);
    assert_eq!(
        h.get(
            &sender,
            Some(bob),
            &format!("/v1/messaging/principals/{alice}/devices")
        )
        .await
        .code(),
        "INSUFFICIENT_SCOPE"
    );
    // Rate limit per credential (5 / minute; 2 used above).
    let mut last = StatusCode::OK;
    for _ in 0..4 {
        last = h
            .get(
                &c,
                Some(bob),
                &format!("/v1/messaging/principals/{alice}/devices"),
            )
            .await
            .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn key_package_claims_are_single_use_under_concurrency() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let a = h.active_device(&c, alice).await;
    let b = h.active_device(&c, alice).await;
    h.publish(
        &c,
        alice,
        a.device_id(),
        &[a.key_package().unwrap(), a.key_package().unwrap()],
    )
    .await;
    h.publish(&c, alice, b.device_id(), &[b.key_package().unwrap()])
        .await;
    let env = c.context.scope.environment_id;
    let now = h.now();
    let mut handles = Vec::new();
    for _ in 0..8 {
        let db = h.conn().clone();
        handles.push(tokio::spawn(async move {
            use sea_orm::TransactionTrait;
            let txn = db.begin().await.unwrap();
            let got = messaging::devices::claim_key_packages(
                &txn,
                env,
                alice,
                Uuid::new_v4(),
                now,
                chrono::Duration::minutes(5),
            )
            .await
            .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            txn.commit().await.unwrap();
            got
        }));
    }
    let mut claimed = Vec::new();
    for handle in handles {
        claimed.extend(handle.await.unwrap().into_iter().map(|k| k.id));
    }
    let unique: std::collections::BTreeSet<_> = claimed.iter().collect();
    assert_eq!(
        claimed.len(),
        unique.len(),
        "a KeyPackage was handed out twice"
    );
    assert_eq!(claimed.len(), 3);
    assert_eq!(
        h.scalar("SELECT count(*) FROM messaging_key_packages WHERE status = 'RESERVED'")
            .await,
        3
    );
    // A reserved package cannot go back to AVAILABLE.
    assert!(h.sql("UPDATE messaging_key_packages SET status = 'AVAILABLE', reservation_id = NULL, reserved_at = NULL").await.is_err());
}

#[tokio::test]
async fn e2ee_device_operations_can_be_disabled_by_deployment() {
    let Some(h) = Harness::build(|s| {
        s.devices.enabled = false;
    })
    .await
    else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let r = h.challenge(&c, alice).await;
    assert_eq!(
        (r.status, r.code(), r.reason()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "FEATURE_DISABLED",
            "E2EE_DEVICES_DISABLED"
        )
    );
}

#[tokio::test]
async fn no_private_key_or_secret_material_reaches_the_database() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&[SEND]);
    let alice = h.principal(&c);
    let device = h.active_device(&c, alice).await;
    h.publish(
        &c,
        alice,
        device.device_id(),
        &[device.key_package().unwrap()],
    )
    .await;
    let dump = h.dump().await.to_lowercase();
    assert!(!dump.contains(&c.token.to_lowercase()), "credential stored");
    for term in ["private", "secret", "-----begin"] {
        assert!(!dump.contains(term), "{term} in the database");
    }
    // No column in the schema could hold one.
    let suspicious = h
        .scalar(
            "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' \
             AND column_name ~ '(private|secret|plaintext|epoch|body|content|recovery|attachment_key)'",
        )
        .await;
    assert_eq!(suspicious, 0);
}
