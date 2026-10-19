//! Device-to-device history transfer (ADR 0052 AD-07) through the real router and
//! PostgreSQL, with REAL client device keys (packages/messaging-crypto `client`): the new
//! device requests, the existing device verifies it against the key directory and seals,
//! the new device opens. Messaging relays opaque bytes only — no server key, no escrow —
//! and refuses forged requests, forged seals, other principals, oversized payloads, and
//! keeps nothing after completion, cancellation, revocation or expiry.

mod common;

use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use cf_messaging_crypto::{
    device::Device,
    history_transfer::{self as ht, TransferContext, TransferKeys},
};
use common::{flows::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

fn ctx(c: &Customer, principal: Uuid, id: Uuid, source: Uuid, target: Uuid) -> TransferContext {
    TransferContext {
        transfer_id: id,
        tenant_id: c.context.scope.tenant_id,
        application_id: c.context.scope.application_id,
        environment_id: c.context.scope.environment_id,
        principal_id: principal,
        source_device_id: source,
        target_device_id: target,
    }
}

struct Req {
    body: Value,
    tc: TransferContext,
    keys: TransferKeys,
}

fn request(
    h: &Harness,
    c: &Customer,
    p: Uuid,
    source: &Device,
    target: &Device,
    signer: &Device,
) -> Req {
    let id = Uuid::new_v4();
    let tc = ctx(c, p, id, source.device_id(), target.device_id());
    let keys = TransferKeys::generate().unwrap();
    let expires = h.now() + chrono::Duration::minutes(30);
    let expires = chrono::DateTime::from_timestamp(expires.timestamp(), 0).unwrap();
    let sig = ht::sign_request(signer, &tc, &keys, expires.timestamp()).unwrap();
    Req {
        body: json!({ "transferId": id, "sourceDeviceId": source.device_id(), "targetDeviceId": target.device_id(),
            "transferPublicKey": B64.encode(keys.public_key()), "expiresAt": expires.to_rfc3339(),
            "requestSignature": B64.encode(sig) }),
        tc,
        keys,
    }
}

async fn directory_key(h: &Harness, c: &Customer, p: Uuid, device: Uuid) -> Vec<u8> {
    let r = h
        .get(c, Some(p), &format!("/v1/messaging/principals/{p}/devices"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let d = r.json["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["deviceId"] == json!(device))
        .expect("device in the directory")
        .clone();
    B64.decode(d["signaturePublicKey"].as_str().unwrap())
        .unwrap()
}

#[tokio::test]
async fn a_new_device_receives_history_sealed_by_the_old_one_and_the_server_learns_nothing() {
    let Some(h) = Harness::build(|s| s.devices.history_transfer_max_bytes = 64 * 1024).await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let alice = h.principal(&c);
    let old = h.active_device(&c, alice).await;
    let new = h.active_device(&c, alice).await;
    let base = "/v1/messaging/history-transfers";

    // The NEW device requests (signed with its device key).
    let req = request(&h, &c, alice, &old, &new, &new);
    let r = h.post(&c, Some(alice), base, req.body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/HistoryTransfer",
        &r.json,
    );
    assert_eq!(r.json["status"], "REQUESTED");
    let id = r.json["transferId"].as_str().unwrap().to_string();
    // A replay of the same signed request is idempotent; a second one for the device is not.
    let again = h.post(&c, Some(alice), base, req.body.clone()).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.text);
    let other = request(&h, &c, alice, &old, &new, &new);
    let dup = h.post(&c, Some(alice), base, other.body).await;
    assert_eq!(
        (dup.status, dup.reason()),
        (StatusCode::CONFLICT, "TRANSFER_ALREADY_OPEN"),
        "{}",
        dup.text
    );

    // The OLD device lists it, verifies it against the directory key and compares codes.
    let list = h
        .get(
            &c,
            Some(alice),
            &format!("{base}?deviceId={}", old.device_id()),
        )
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.text);
    let seen = list.json["transfers"][0].clone();
    let target_key = directory_key(&h, &c, alice, new.device_id()).await;
    let source_key = directory_key(&h, &c, alice, old.device_id()).await;
    let tpk = B64
        .decode(seen["transferPublicKey"].as_str().unwrap())
        .unwrap();
    let expires =
        chrono::DateTime::parse_from_rfc3339(seen["expiresAt"].as_str().unwrap()).unwrap();
    assert!(ht::verify_request(
        &target_key,
        &req.tc,
        &tpk,
        expires.timestamp(),
        &B64.decode(seen["requestSignature"].as_str().unwrap())
            .unwrap()
    ));
    let code_on_old = ht::comparison_code(&req.tc, &tpk, &source_key, &target_key);
    let code_on_new = ht::comparison_code(
        &req.tc,
        req.keys.public_key(),
        &old.signature_public_key(),
        &new.signature_public_key(),
    );
    assert_eq!(
        code_on_old, code_on_new,
        "the user sees the same code on both devices"
    );

    // A seal signed by the wrong device is refused; the real one is stored opaquely.
    let history: Vec<u8> = b"HISTORY-ARCHIVE:"
        .iter()
        .copied()
        .chain((0..20_000).map(|i| (i % 251) as u8))
        .collect();
    let forged = ht::seal_history(&new, &req.tc, &tpk, &history).unwrap();
    let path = format!("{base}/{id}/sealed-history");
    let r = h
        .put(
            &c,
            Some(alice),
            &path,
            json!({ "sealedHistory": B64.encode(&forged) }),
        )
        .await;
    assert_eq!(r.reason(), "SEALED_HISTORY_SIGNATURE_INVALID", "{}", r.text);
    let sealed = ht::seal_history(&old, &req.tc, &tpk, &history).unwrap();
    let r = h
        .put(
            &c,
            Some(alice),
            &path,
            json!({ "sealedHistory": B64.encode(&sealed) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "SEALED");
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_history_transfers WHERE id = '{id}' AND position('\\x484953544f52592d41524348495645'::bytea in sealed_history) > 0")).await,
        0,
        "the server holds ciphertext only"
    );
    // Nothing that could open it is stored anywhere: the only key column is PUBLIC.
    assert_eq!(
        h.scalar("SELECT count(*) FROM information_schema.columns WHERE table_name = 'messaging_history_transfers' AND column_name ~ '(private|secret|recovery|escrow)'").await,
        0
    );

    // Another principal sees nothing and can do nothing.
    let bob = h.principal(&c);
    let _bob_device = h.active_device(&c, bob).await;
    for r in [
        h.get(&c, Some(bob), &format!("{base}/{id}")).await,
        h.get(&c, Some(bob), &path).await,
        h.post(&c, Some(bob), &format!("{base}/{id}/complete"), json!({}))
            .await,
    ] {
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    }

    // The NEW device downloads and opens it; completion deletes the sealed bytes.
    let r = h.get(&c, Some(alice), &path).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let got = B64
        .decode(r.json["sealedHistory"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        ht::open_history(&req.keys, &source_key, &req.tc, &got).unwrap(),
        history
    );
    let r = h
        .post(&c, Some(alice), &format!("{base}/{id}/complete"), json!({}))
        .await;
    assert_eq!(
        (r.status, r.json["status"].as_str()),
        (StatusCode::OK, Some("COMPLETED")),
        "{}",
        r.text
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_history_transfers WHERE id = '{id}' AND sealed_history IS NULL")).await,
        1
    );
    let r = h.get(&c, Some(alice), &path).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    println!(
        "EVIDENCE history_transfer sealed_bytes={} plaintext_bytes={} server_keys=0",
        sealed.len(),
        history.len()
    );
}

#[tokio::test]
async fn forged_requests_foreign_devices_oversize_revocation_and_expiry() {
    let Some(h) = Harness::build(|s| s.devices.history_transfer_max_bytes = 64 * 1024).await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let alice = h.principal(&c);
    let bob = h.principal(&c);
    let a1 = h.active_device(&c, alice).await;
    let a2 = h.active_device(&c, alice).await;
    let b1 = h.active_device(&c, bob).await;
    let base = "/v1/messaging/history-transfers";

    // Signed by the SOURCE instead of the target: refused.
    let forged = request(&h, &c, alice, &a1, &a2, &a1);
    let r = h.post(&c, Some(alice), base, forged.body).await;
    assert_eq!(r.reason(), "TRANSFER_SIGNATURE_INVALID", "{}", r.text);
    // Another principal's device as the source: NOT_FOUND (indistinguishable).
    let foreign = request(&h, &c, alice, &b1, &a2, &a2);
    let r = h.post(&c, Some(alice), base, foreign.body).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // An expiry beyond one hour: refused.
    let mut long = request(&h, &c, alice, &a1, &a2, &a2);
    long.body["expiresAt"] = json!((h.now() + chrono::Duration::hours(3)).to_rfc3339());
    let r = h.post(&c, Some(alice), base, long.body).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // Oversized sealed history: refused, nothing stored.
    let req = request(&h, &c, alice, &a1, &a2, &a2);
    let r = h.post(&c, Some(alice), base, req.body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["transferId"].as_str().unwrap().to_string();
    let big =
        ht::seal_history(&a1, &req.tc, req.keys.public_key(), &vec![7u8; 100 * 1024]).unwrap();
    let path = format!("{base}/{id}/sealed-history");
    let r = h
        .put(
            &c,
            Some(alice),
            &path,
            json!({ "sealedHistory": B64.encode(&big) }),
        )
        .await;
    assert_eq!(r.reason(), "SEALED_HISTORY_TOO_LARGE", "{}", r.text);
    // Revoking the source device ends the transfer and deletes what it sealed.
    let sealed = ht::seal_history(&a1, &req.tc, req.keys.public_key(), b"small history").unwrap();
    let r = h
        .put(
            &c,
            Some(alice),
            &path,
            json!({ "sealedHistory": B64.encode(&sealed) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/messaging/devices/{}/revoke", a1.device_id()),
            json!({ "reason": "LOST_OR_STOLEN" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let t = h.get(&c, Some(alice), &format!("{base}/{id}")).await;
    assert_eq!(
        (t.json["status"].as_str(), t.json["endReason"].as_str()),
        (Some("CANCELLED"), Some("DEVICE_REVOKED")),
        "{}",
        t.text
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_history_transfers WHERE id = '{id}' AND sealed_history IS NULL")).await,
        1
    );

    // Expiry: a transfer left open past its window is EXPIRED and its bytes deleted.
    let a3 = h.active_device(&c, alice).await;
    let req = request(&h, &c, alice, &a2, &a3, &a3);
    let r = h.post(&c, Some(alice), base, req.body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["transferId"].as_str().unwrap().to_string();
    let sealed = ht::seal_history(&a2, &req.tc, req.keys.public_key(), b"history").unwrap();
    let path = format!("{base}/{id}/sealed-history");
    h.put(
        &c,
        Some(alice),
        &path,
        json!({ "sealedHistory": B64.encode(&sealed) }),
    )
    .await;
    h.clock.advance(chrono::Duration::minutes(31));
    let r = h.get(&c, Some(alice), &path).await;
    assert_eq!(r.reason(), "TRANSFER_EXPIRED", "{}", r.text);
    assert_eq!(
        messaging::devices::history_transfer::expire_due(&h.state)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        h.scalar(&format!("SELECT count(*) FROM messaging_history_transfers WHERE id = '{id}' AND status = 'EXPIRED' AND sealed_history IS NULL")).await,
        1
    );
    // A cancelled transfer frees the target for a new one.
    let req2 = request(&h, &c, alice, &a2, &a3, &a3);
    let r = h.post(&c, Some(alice), base, req2.body).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = h
        .post(
            &c,
            Some(alice),
            &format!("{base}/{}/cancel", r.json["transferId"].as_str().unwrap()),
            json!({ "reason": "CODES_DIFFERED" }),
        )
        .await;
    assert_eq!(r.json["status"], "CANCELLED", "{}", r.text);
}
