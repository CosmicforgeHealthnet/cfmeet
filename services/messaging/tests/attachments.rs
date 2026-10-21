//! Part F — attachments (§69–§80) through the real router and PostgreSQL.
//!
//! * MANAGED: upload intent → signed upload bound to size / digest / type → finalization
//!   re-verifies the stored object → content-type checks → self-hosted scan → READY;
//!   messages reference READY attachments; downloads re-authorize every time.
//! * PRIVATE_E2EE: a REAL client device encrypts the file before upload
//!   (`cf_messaging_crypto::attachment`), sends the descriptor inside a REAL MLS message,
//!   and the recipient device decrypts it locally. The server never sees the key, the
//!   file name or the media type, and never claims a malware scan.
//! * Quotas (atomic under concurrency), lifecycle workers, legal holds, tenant isolation.
//!
//! Object storage is the in-memory store with the same signed-upload checks as S3
//! (`MemoryObjectStore::upload_signed`); `real_object_storage_end_to_end` repeats the
//! flow against a real S3-compatible server (MinIO in CI) when
//! `OBJECT_STORAGE_TEST_ENDPOINT` is set, and `real_clamav_detects_eicar` runs a real
//! clamd when `MESSAGING_TEST_CLAMAV_ADDRESS` is set.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use axum::http::{Method, StatusCode};
use cf_messaging_crypto::{
    attachment::{self, AttachmentDescriptor},
    binding,
    group::Processed,
};
use cf_object_storage::{sha256_hex, MemoryObjectStore, ObjectStore, SignedWrite};
use common::{flows::*, *};
use messaging::attachments::{
    lifecycle,
    scanner::{AttachmentScanner, ScanOutcome},
};
use serde_json::{json, Value};
use uuid::Uuid;

/// A scanner whose verdict the test controls (counts every scan it is asked for).
struct ScriptedScanner {
    verdict: Mutex<ScanOutcome>,
    calls: AtomicUsize,
}

impl ScriptedScanner {
    fn new(v: ScanOutcome) -> Arc<Self> {
        Arc::new(Self {
            verdict: Mutex::new(v),
            calls: AtomicUsize::new(0),
        })
    }
    fn set(&self, v: ScanOutcome) {
        *self.verdict.lock().unwrap() = v;
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl AttachmentScanner for ScriptedScanner {
    fn engine(&self) -> &'static str {
        "scripted-test"
    }
    async fn scan(&self, _bytes: &[u8]) -> ScanOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.verdict.lock().unwrap().clone()
    }
}

struct Env {
    h: Harness,
    store: Arc<MemoryObjectStore>,
    scanner: Arc<ScriptedScanner>,
}

async fn env() -> Option<Env> {
    let store = Arc::new(MemoryObjectStore::new());
    let scanner = ScriptedScanner::new(ScanOutcome::Clean);
    let (s2, sc2) = (store.clone(), scanner.clone());
    let h = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = sc2;
    })
    .await?;
    Some(Env { h, store, scanner })
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";

fn png(n: usize) -> Vec<u8> {
    let mut b = PNG.to_vec();
    b.extend((0..n).map(|i| (i % 251) as u8));
    b
}

fn write_of(upload: &Value) -> SignedWrite {
    SignedWrite {
        method: "PUT",
        url: upload["url"].as_str().unwrap().to_string(),
        headers: upload["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["name"].as_str().unwrap().to_string(),
                    h["value"].as_str().unwrap().to_string(),
                )
            })
            .collect(),
        expires_at: chrono::DateTime::parse_from_rfc3339(upload["expiresAt"].as_str().unwrap())
            .unwrap()
            .into(),
    }
}

fn cid(conv: &Value) -> String {
    conv["conversationId"].as_str().unwrap().to_string()
}

impl Env {
    /// Lets MANAGED groups exist and enables attachments (plus `extra` overrides).
    async fn policy(&self, c: &Customer, attachments: Value) {
        let mut a = json!({ "enabled": true });
        a.as_object_mut()
            .unwrap()
            .extend(attachments.as_object().unwrap().clone());
        self.h
            .set_policy(
                c,
                json!({
                    "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
                    "attachments": a,
                }),
            )
            .await;
    }

    async fn managed_group(&self, c: &Customer, n: usize) -> (Value, Vec<Uuid>) {
        let people: Vec<Uuid> = (0..n).map(|_| self.h.principal(c)).collect();
        let r = self
            .h
            .create_conversation(
                c,
                people[0],
                json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let id = cid(&r.json);
        for p in &people[1..] {
            let a = self
                .h
                .post(
                    c,
                    Some(people[0]),
                    &format!("/v1/conversations/{id}/members"),
                    json!({ "principalId": p }),
                )
                .await;
            assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
        }
        (r.json, people)
    }

    async fn intent(&self, c: &Customer, conv: &Value, who: Uuid, body: Value) -> Response {
        self.h
            .post(
                c,
                Some(who),
                &format!("/v1/conversations/{}/attachments", cid(conv)),
                body,
            )
            .await
    }

    async fn finalize(&self, c: &Customer, conv: &Value, who: Uuid, id: &Value) -> Response {
        self.h
            .post(
                c,
                Some(who),
                &format!(
                    "/v1/conversations/{}/attachments/{}/finalize",
                    cid(conv),
                    id.as_str().unwrap()
                ),
                json!({}),
            )
            .await
    }

    async fn grant(&self, c: &Customer, conv: &Value, who: Uuid, id: &Value) -> Response {
        self.h
            .post(
                c,
                Some(who),
                &format!(
                    "/v1/conversations/{}/attachments/{}/download-grants",
                    cid(conv),
                    id.as_str().unwrap()
                ),
                json!({}),
            )
            .await
    }

    async fn content(&self, c: &Customer, conv: &Value, who: Uuid, id: &Value) -> Response {
        self.h
            .get(
                c,
                Some(who),
                &format!(
                    "/v1/conversations/{}/attachments/{}/content",
                    cid(conv),
                    id.as_str().unwrap()
                ),
            )
            .await
    }

    /// Intent + signed upload + finalize of a MANAGED file.
    #[allow(clippy::too_many_arguments)]
    async fn managed_upload(
        &self,
        c: &Customer,
        conv: &Value,
        who: Uuid,
        kind: &str,
        media_type: &str,
        name: &str,
        bytes: &[u8],
    ) -> Response {
        let r = self
            .intent(
                c,
                conv,
                who,
                json!({ "kind": kind, "size": bytes.len(), "sha256": sha256_hex(bytes), "mediaType": media_type, "fileName": name }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        self.store
            .upload_signed(&write_of(&r.json["upload"]), self.h.now(), bytes.to_vec())
            .unwrap();
        self.finalize(c, conv, who, &r.json["attachmentId"]).await
    }

    async fn usage(&self, c: &Customer) -> (i64, i64) {
        let env = c.context.scope.environment_id;
        (
            self.h
                .scalar(&format!(
                    "SELECT COALESCE((SELECT committed_bytes FROM messaging_storage_usage WHERE environment_id = '{env}'), 0)"
                ))
                .await,
            self.h
                .scalar(&format!(
                    "SELECT COALESCE((SELECT reserved_bytes FROM messaging_storage_usage WHERE environment_id = '{env}'), 0)"
                ))
                .await,
        )
    }
}

#[tokio::test]
async fn managed_attachment_lifecycle_and_authorized_downloads() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 3).await;
    let (alice, bob, carol) = (people[0], people[1], people[2]);
    let id = cid(&conv);
    let bytes = png(5000);

    let intent = e
        .intent(
            &c,
            &conv,
            alice,
            json!({ "kind": "IMAGE", "size": bytes.len(), "sha256": sha256_hex(&bytes), "mediaType": "image/png", "fileName": "../../etc/Holiday\u{0}.png" }),
        )
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/AttachmentUploadIntent",
        &intent.json,
    );
    assert_eq!(intent.json["state"], "INTENT_CREATED");
    assert_eq!(
        intent.json["fileName"], "Holiday.png",
        "sanitized presentation name"
    );
    assert_eq!(intent.json["scan"]["status"], "PENDING");
    let aid = intent.json["attachmentId"].clone();
    // The storage key is server-generated (never the client's file name).
    let key = e
        .h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_attachments WHERE id = '{}' AND object_key = 'att/{}/{}/{}'",
            aid.as_str().unwrap(),
            c.context.scope.environment_id.simple(),
            uuid(&conv["conversationId"]).simple(),
            uuid(&aid).simple()
        ))
        .await;
    assert_eq!(key, 1);
    // The signed upload is bound to the declared bytes.
    let w = write_of(&intent.json["upload"]);
    assert!(
        e.store.upload_signed(&w, e.h.now(), png(4999)).is_err(),
        "other size"
    );
    let mut tampered = bytes.clone();
    tampered[100] ^= 1;
    assert!(
        e.store.upload_signed(&w, e.h.now(), tampered).is_err(),
        "other digest"
    );
    // Not downloadable before finalization; nobody else can see an unsent upload.
    let r = e.grant(&c, &conv, alice, &aid).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("ATTACHMENT_NOT_READY", "NOT_FINALIZED"),
        "{}",
        r.text
    );
    let r =
        e.h.get(
            &c,
            Some(bob),
            &format!(
                "/v1/conversations/{id}/attachments/{}",
                aid.as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Finalizing before the upload: nothing to verify.
    let r = e.finalize(&c, &conv, alice, &aid).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("ATTACHMENT_NOT_READY", "UPLOAD_MISSING"),
        "{}",
        r.text
    );
    e.store.upload_signed(&w, e.h.now(), bytes.clone()).unwrap();
    // Only the uploader finalizes.
    let r = e.finalize(&c, &conv, bob, &aid).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = e.finalize(&c, &conv, alice, &aid).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "READY");
    assert_eq!(r.json["scan"]["status"], "CLEAN");
    assert_eq!(r.json["scan"]["engine"], "scripted-test");
    assert_eq!(e.scanner.calls(), 1);
    // Idempotent.
    let again = e.finalize(&c, &conv, alice, &aid).await;
    assert_eq!(again.json["state"], "READY");
    assert_eq!(e.scanner.calls(), 1);
    assert_eq!(
        e.usage(&c).await,
        (bytes.len() as i64, 0),
        "reservation committed"
    );
    // Metered once as stored bytes (a usage fact, no price).
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT COALESCE(sum(quantity), 0)::bigint FROM messaging_usage_records WHERE usage_type = 'ATTACHMENT_STORED' AND subject_id = '{}'",
            aid.as_str().unwrap()
        ))
        .await,
        bytes.len() as i64
    );

    // Only the sender's READY attachment in this conversation can be referenced.
    let r = e
        .h
        .post(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "not mine", "attachmentIds": [aid] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let sent = e
        .h
        .post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "our holiday", "attachmentIds": [aid] }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    assert_eq!(sent.json["attachmentIds"], json!([aid]));
    let reuse =
        e.h.post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "again", "attachmentIds": [aid] }),
        )
        .await;
    assert_eq!(
        (reuse.code(), reuse.reason()),
        ("CONFLICT", "ATTACHMENT_ALREADY_USED"),
        "{}",
        reuse.text
    );

    // Members download through short-lived grants or the re-authorized proxied read.
    let g = e.grant(&c, &conv, bob, &aid).await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/AttachmentDownloadGrant",
        &g.json,
    );
    assert_eq!(g.json["residualAccessSeconds"], 60);
    let url = g.json["url"].as_str().unwrap();
    assert!(
        url.contains("response-content-disposition=attachment"),
        "{url}"
    );
    let body = e.content(&c, &conv, bob, &aid).await;
    assert_eq!(body.status, StatusCode::OK, "{}", body.text);
    assert_eq!(body.bytes, bytes);
    assert_eq!(body.headers["x-content-type-options"], "nosniff");
    assert_eq!(body.headers["cache-control"], "no-store");
    assert_eq!(body.headers["content-type"], "image/png");
    assert!(body.headers["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment; filename=\"Holiday.png\""));

    // A non-member, another tenant, a missing scope: no access, no existence oracle.
    let outsider = e.h.principal(&c);
    assert_eq!(
        e.grant(&c, &conv, outsider, &aid).await.status,
        StatusCode::NOT_FOUND
    );
    let other = e.h.customer(ALL_SCOPES);
    let r = e.grant(&other, &conv, e.h.principal(&other), &aid).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let reader = e.h.credential(&c, &["messages.read"]);
    let r = e.grant(&reader, &conv, bob, &aid).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);

    // A removed member gets no fresh grant and no proxied bytes.
    let rm =
        e.h.delete(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/members/{carol}"),
        )
        .await;
    assert!(rm.status.is_success(), "{}", rm.text);
    assert_eq!(
        e.grant(&c, &conv, carol, &aid).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        e.content(&c, &conv, carol, &aid).await.status,
        StatusCode::NOT_FOUND
    );

    // A deleted message releases no bytes (its attachment follows the message).
    let mid = sent.json["messageId"].as_str().unwrap();
    let d =
        e.h.delete(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/messages/{mid}"),
        )
        .await;
    assert!(d.status.is_success(), "{}", d.text);
    let r = e.grant(&c, &conv, bob, &aid).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("ATTACHMENT_NOT_READY", "MESSAGE_NOT_ACTIVE"),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn private_e2ee_attachment_round_trip_with_real_client_encryption() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, mut alice, mut bob) = e.h.e2ee_direct(&c).await;
    let id = cid(&conv);
    let secret = b"%PDF-1.7 passport scan of Alice, secret ".repeat(4000);

    // The device encrypts BEFORE upload; the intent declares the ciphertext only.
    let enc = attachment::encrypt_attachment(&secret, "application/pdf", "passport.pdf").unwrap();
    assert_eq!(
        enc.ciphertext.len() as u64,
        attachment::ciphertext_size(secret.len() as u64)
    );
    // Protected metadata in an E2EE intent is refused.
    for leak in [
        json!({ "fileName": "passport.pdf" }),
        json!({ "mediaType": "application/pdf" }),
        json!({ "durationMs": 1000 }),
    ] {
        let mut body = json!({ "kind": "DOCUMENT", "size": enc.ciphertext.len(), "sha256": enc.descriptor.ciphertext_sha256,
            "deviceId": alice.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 });
        body.as_object_mut()
            .unwrap()
            .extend(leak.as_object().unwrap().clone());
        let r = e.intent(&c, &conv, alice.principal, body).await;
        assert_eq!(
            (r.code(), r.reason()),
            ("CONTENT_REJECTED", "PROTECTED_METADATA_IN_E2EE"),
            "{}",
            r.text
        );
    }
    let r = e
        .intent(&c, &conv, alice.principal, json!({ "kind": "DOCUMENT", "size": enc.ciphertext.len(), "sha256": enc.descriptor.ciphertext_sha256, "deviceId": alice.device_id(), "cryptoFormat": "AES_CBC_HOMEBREW" }))
        .await;
    assert_eq!(r.reason(), "UNSUPPORTED_CRYPTO_FORMAT", "{}", r.text);
    // Another principal's device cannot be named.
    let r = e
        .intent(&c, &conv, alice.principal, json!({ "kind": "DOCUMENT", "size": enc.ciphertext.len(), "sha256": enc.descriptor.ciphertext_sha256, "deviceId": bob.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 }))
        .await;
    assert!(r.status.is_client_error(), "{}", r.text);

    let intent = e
        .intent(&c, &conv, alice.principal, json!({ "kind": "DOCUMENT", "size": enc.ciphertext.len(), "sha256": enc.descriptor.ciphertext_sha256, "deviceId": alice.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 }))
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    assert_eq!(intent.json["mediaType"], "application/octet-stream");
    assert_eq!(intent.json["fileName"], Value::Null);
    assert_eq!(intent.json["scan"]["status"], "NOT_APPLICABLE_ENCRYPTED");
    let aid = intent.json["attachmentId"].clone();
    e.store
        .upload_signed(
            &write_of(&intent.json["upload"]),
            e.h.now(),
            enc.ciphertext.clone(),
        )
        .unwrap();
    let r = e.finalize(&c, &conv, alice.principal, &aid).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "READY");
    assert_eq!(
        r.json["scan"]["status"], "NOT_APPLICABLE_ENCRYPTED",
        "never claimed scanned"
    );
    assert_eq!(r.json["scan"]["engine"], Value::Null);
    assert_eq!(
        e.scanner.calls(),
        0,
        "E2EE ciphertext is never sent to a scanner"
    );

    // The descriptor (key, digest, name, type) travels only inside the MLS message.
    let descriptor = enc.descriptor.clone().for_attachment(uuid(&aid));
    let client_message_id = Uuid::new_v4();
    let ct = e.h.encrypt_for(
        &c,
        &conv,
        &mut alice,
        client_message_id,
        &b64(&descriptor.to_bytes()),
    );
    let sent = e
        .h
        .post(
            &c,
            Some(alice.principal),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": client_message_id, "deviceId": alice.device_id(),
                    "epoch": alice.group.as_ref().unwrap().epoch(), "ciphertext": b64(&ct), "attachmentIds": [aid] }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);

    // Nothing the server stores reveals the key, the name or the plaintext.
    let dump = e.h.dump().await;
    assert!(!dump.contains("passport"), "file name leaked");
    assert!(
        !dump.contains(&hex::encode(descriptor.key.as_bytes())),
        "key leaked"
    );
    let stored = e.store.get_raw(&format!(
        "att/{}/{}/{}",
        c.context.scope.environment_id.simple(),
        uuid(&conv["conversationId"]).simple(),
        uuid(&aid).simple()
    ));
    let stored = stored.expect("object stored");
    assert!(
        !stored.windows(8).any(|w| w == b"passport"),
        "plaintext in storage"
    );

    // Bob's device decrypts the message, fetches the ciphertext and decrypts it locally.
    let r =
        e.h.get(
            &c,
            Some(bob.principal),
            &format!(
                "/v1/conversations/{id}/messages?afterSequence={}",
                bob.cursor
            ),
        )
        .await;
    let msg = r.json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["messageId"] == sent.json["messageId"])
        .unwrap()
        .clone();
    assert_eq!(msg["attachmentIds"], json!([aid]));
    let group = bob.group.as_mut().unwrap();
    let Processed::Application { plaintext, aad } = bob
        .device
        .process(group, &unb64(&msg["e2ee"]["ciphertext"]))
        .unwrap()
    else {
        panic!("expected an application message")
    };
    assert!(binding::parse_application_message_aad(&aad).is_some());
    let received =
        AttachmentDescriptor::from_bytes(&unb64(&json!(String::from_utf8(plaintext).unwrap())))
            .unwrap();
    assert_eq!(received.attachment_id, uuid(&aid));
    let g = e.grant(&c, &conv, bob.principal, &aid).await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);
    assert_eq!(g.json["sha256"], json!(received.ciphertext_sha256));
    assert_eq!(g.json["cryptoFormat"], attachment::ATTACHMENT_FORMAT_V1);
    assert!(
        g.json["url"].as_str().unwrap().contains("attachment.bin"),
        "no real name in the grant"
    );
    let body = e.content(&c, &conv, bob.principal, &aid).await;
    assert_eq!(body.status, StatusCode::OK);
    assert_eq!(body.headers["content-type"], "application/octet-stream");
    assert_eq!(
        attachment::decrypt_attachment(&received, &body.bytes).unwrap(),
        secret
    );
    // A wrong key (e.g. a forged descriptor) cannot decrypt.
    let mut forged = received.clone();
    forged.key = attachment::AttachmentKey::generate();
    assert!(attachment::decrypt_attachment(&forged, &body.bytes).is_err());
}

#[tokio::test]
async fn integrity_and_content_type_security() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let alice = people[0];

    // An object swapped behind the signed upload (store-side tampering) is rejected.
    let bytes = png(100);
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": bytes.len(), "sha256": sha256_hex(&bytes), "mediaType": "image/png" }))
        .await;
    let aid = r.json["attachmentId"].clone();
    let key = format!(
        "att/{}/{}/{}",
        c.context.scope.environment_id.simple(),
        uuid(&conv["conversationId"]).simple(),
        uuid(&aid).simple()
    );
    let mut other = bytes.clone();
    other[50] ^= 0xff;
    e.store.put_raw(&key, other);
    let f = e.finalize(&c, &conv, alice, &aid).await;
    assert_eq!(
        (f.code(), f.reason()),
        ("INTEGRITY_CHECK_FAILED", "DIGEST_MISMATCH"),
        "{}",
        f.text
    );
    let row =
        e.h.get(
            &c,
            Some(alice),
            &format!(
                "/v1/conversations/{}/attachments/{}",
                cid(&conv),
                aid.as_str().unwrap()
            ),
        )
        .await;
    assert_eq!(row.json["state"], "REJECTED");
    assert_eq!(row.json["stateReason"], "DIGEST_MISMATCH");
    assert_eq!(e.usage(&c).await, (0, 0), "reservation released");
    // The rejected object is deleted by the lifecycle worker.
    assert!(e.store.get_raw(&key).is_some());
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert!(e.store.get_raw(&key).is_none());
    // Size mismatch.
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": 500, "sha256": sha256_hex(&png(100)), "mediaType": "image/png" }))
        .await;
    let aid = r.json["attachmentId"].clone();
    e.store.put_raw(
        &format!(
            "att/{}/{}/{}",
            c.context.scope.environment_id.simple(),
            uuid(&conv["conversationId"]).simple(),
            uuid(&aid).simple()
        ),
        png(100),
    );
    let f = e.finalize(&c, &conv, alice, &aid).await;
    assert_eq!(f.reason(), "SIZE_MISMATCH", "{}", f.text);

    // MIME confusion: an executable declared as an image.
    let exe = b"MZ\x90\x00\x03\x00\x00\x00 this program cannot be run in DOS mode".to_vec();
    let f = e
        .managed_upload(&c, &conv, alice, "IMAGE", "image/png", "cat.png", &exe)
        .await;
    assert_eq!(f.code(), "CONTENT_REJECTED", "{}", f.text);
    // Active web content declared as text.
    let html = b"<html><script>alert(1)</script></html>".to_vec();
    let f = e
        .managed_upload(
            &c,
            &conv,
            alice,
            "DOCUMENT",
            "text/plain",
            "notes.txt",
            &html,
        )
        .await;
    assert_eq!(f.code(), "CONTENT_REJECTED", "{}", f.text);
    // Refused at intent time: executable names (incl. double extensions), disallowed types.
    for (kind, mt, name) in [
        ("DOCUMENT", "application/pdf", "invoice.pdf.exe"),
        ("DOCUMENT", "application/pdf", "invoice.exe.pdf"),
        ("FILE", "application/octet-stream", "setup.msi"),
        ("IMAGE", "image/svg+xml", "logo.svg"),
        ("DOCUMENT", "text/html", "page.html"),
    ] {
        let r = e
            .intent(&c, &conv, alice, json!({ "kind": kind, "size": 10, "sha256": sha256_hex(b"0123456789"), "mediaType": mt, "fileName": name }))
            .await;
        assert_eq!(r.code(), "CONTENT_REJECTED", "{name}: {}", r.text);
    }
    // Over the policy size; a disallowed kind.
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": 26 * 1024 * 1024, "sha256": sha256_hex(b"x"), "mediaType": "image/png" }))
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("QUOTA_EXCEEDED", "ATTACHMENT_TOO_LARGE")
    );
    // Voice notes declare a bounded duration (marked unverified).
    let ogg = {
        let mut b = b"OggS\x00\x02".to_vec();
        b.extend([7u8; 200]);
        b
    };
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "VOICE_NOTE", "size": ogg.len(), "sha256": sha256_hex(&ogg), "mediaType": "audio/ogg", "durationMs": 901_000 }))
        .await;
    assert_eq!(r.reason(), "DURATION_NOT_ALLOWED", "{}", r.text);
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "VOICE_NOTE", "size": ogg.len(), "sha256": sha256_hex(&ogg), "mediaType": "audio/ogg",
            "durationMs": 4200, "codec": "opus", "waveform": [0, 12, 255, 40] }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["media"]["durationMs"], 4200);
    assert_eq!(r.json["media"]["durationVerified"], false);
    e.store
        .upload_signed(&write_of(&r.json["upload"]), e.h.now(), ogg)
        .unwrap();
    let f = e.finalize(&c, &conv, alice, &r.json["attachmentId"]).await;
    assert_eq!(f.json["state"], "READY", "{}", f.text);
}

#[tokio::test]
async fn scanning_fails_closed_and_quarantines() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 2).await;
    let (alice, bob) = (people[0], people[1]);
    let id = cid(&conv);

    // Infected → QUARANTINED: never downloadable, never attachable.
    e.scanner
        .set(ScanOutcome::Infected("Eicar-Test-Signature".into()));
    let f = e
        .managed_upload(
            &c,
            &conv,
            alice,
            "DOCUMENT",
            "text/plain",
            "eicar.txt",
            b"plain text pretending",
        )
        .await;
    assert_eq!(f.status, StatusCode::OK, "{}", f.text);
    assert_eq!(f.json["state"], "QUARANTINED");
    assert_eq!(f.json["scan"]["status"], "INFECTED");
    assert_eq!(f.json["scan"]["finding"], "Eicar-Test-Signature");
    let q = f.json["attachmentId"].clone();
    let r = e.grant(&c, &conv, alice, &q).await;
    assert_eq!(
        (r.code(), r.reason()),
        ("ATTACHMENT_NOT_READY", "QUARANTINED")
    );
    let r = e.content(&c, &conv, alice, &q).await;
    assert_eq!(r.reason(), "QUARANTINED");
    let r =
        e.h.post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "x", "attachmentIds": [q] }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("ATTACHMENT_NOT_READY", "QUARANTINED"),
        "{}",
        r.text
    );

    // Scanner unavailable → VERIFYING (pending), never READY unscanned.
    e.scanner
        .set(ScanOutcome::Unavailable("connection refused".into()));
    let f = e
        .managed_upload(
            &c,
            &conv,
            alice,
            "DOCUMENT",
            "text/plain",
            "notes.txt",
            b"meeting notes, all fine",
        )
        .await;
    assert_eq!(f.json["state"], "VERIFYING", "{}", f.text);
    assert_eq!(f.json["scan"]["status"], "PENDING");
    let p = f.json["attachmentId"].clone();
    let r = e.grant(&c, &conv, alice, &p).await;
    assert_eq!(r.reason(), "SCAN_PENDING");
    // Still unavailable on the retry: still pending.
    e.h.clock.advance(chrono::Duration::seconds(31));
    lifecycle::run_once(&e.h.state).await.unwrap();
    let m =
        e.h.get(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/attachments/{}", p.as_str().unwrap()),
        )
        .await;
    assert_eq!(m.json["state"], "VERIFYING");
    // The scanner recovers: the retry settles READY.
    e.scanner.set(ScanOutcome::Clean);
    e.h.clock.advance(chrono::Duration::seconds(120));
    lifecycle::run_once(&e.h.state).await.unwrap();
    let m =
        e.h.get(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/attachments/{}", p.as_str().unwrap()),
        )
        .await;
    assert_eq!(m.json["state"], "READY", "{}", m.text);
    assert_eq!(m.json["scan"]["status"], "CLEAN");
    let _ = bob;

    // A scanner that never recovers: rejected after the retry budget (never READY).
    e.scanner.set(ScanOutcome::Unavailable("down".into()));
    let f = e
        .managed_upload(
            &c,
            &conv,
            alice,
            "DOCUMENT",
            "text/plain",
            "later.txt",
            b"other notes, fine",
        )
        .await;
    let p = f.json["attachmentId"].clone();
    for _ in 0..12 {
        e.h.clock.advance(chrono::Duration::hours(1));
        lifecycle::run_once(&e.h.state).await.unwrap();
    }
    let m =
        e.h.get(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/attachments/{}", p.as_str().unwrap()),
        )
        .await;
    assert_eq!(m.json["state"], "REJECTED", "{}", m.text);
    assert_eq!(m.json["stateReason"], "SCAN_UNAVAILABLE");

    // Policy may waive scanning for MANAGED: then it is reported NOT_REQUIRED, not CLEAN.
    e.policy(&c, json!({ "managedScanRequired": false })).await;
    let calls = e.scanner.calls();
    let f = e
        .managed_upload(
            &c,
            &conv,
            alice,
            "DOCUMENT",
            "text/plain",
            "unscanned.txt",
            b"no scan by policy",
        )
        .await;
    assert_eq!(f.json["state"], "READY");
    assert_eq!(f.json["scan"]["status"], "NOT_REQUIRED");
    assert_eq!(e.scanner.calls(), calls);
    // §118 / Appendix D: the transitions above recorded their domain events, and every
    // event conforms to the published event contract.
    let types = e.h.conforming_event_types().await;
    for t in [
        "messaging.attachment.quarantined",
        "messaging.attachment.rejected",
        "messaging.attachment.ready",
    ] {
        assert!(types.contains(t), "{t} not recorded: {types:?}");
    }
}

#[tokio::test]
async fn quotas_are_atomic_and_released() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(
        &c,
        json!({ "storageBudgetBytes": 3000, "maxConcurrentUploads": 50 }),
    )
    .await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let alice = people[0];
    // 20 concurrent 1000-byte intents against a 3000-byte budget: exactly 3 win.
    let futures: Vec<_> = (0..20)
        .map(|i| {
            let mut body = png(1000 - PNG.len());
            body[PNG.len()] = i as u8;
            let e = &e;
            let c = &c;
            let conv = &conv;
            async move {
                e.intent(c, conv, alice, json!({ "kind": "IMAGE", "size": body.len(), "sha256": sha256_hex(&body), "mediaType": "image/png" }))
                    .await
            }
        })
        .collect();
    let results = futures_util::future::join_all(futures).await;
    let ok = results
        .iter()
        .filter(|r| r.status == StatusCode::CREATED)
        .count();
    let refused = results
        .iter()
        .filter(|r| r.reason() == "STORAGE_BUDGET")
        .count();
    assert_eq!(
        (ok, refused),
        (3, 17),
        "{:?}",
        results.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
    );
    assert_eq!(e.usage(&c).await, (0, 3000));
    // Expired intents release their reservation.
    e.h.clock.advance(chrono::Duration::minutes(16));
    let report = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(report.expired_intents, 3);
    assert_eq!(e.usage(&c).await, (0, 0));
    // A finalized upload commits; an abandoned one (never sent) is deleted and freed.
    let f = e
        .managed_upload(&c, &conv, alice, "IMAGE", "image/png", "a.png", &png(500))
        .await;
    assert_eq!(f.json["state"], "READY", "{}", f.text);
    let committed = e.usage(&c).await.0;
    assert!(committed > 0);
    e.h.clock.advance(chrono::Duration::hours(25));
    let report = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(report.abandoned, 1);
    assert_eq!(e.usage(&c).await, (0, 0));
    assert!(e.store.is_empty(), "{:?}", e.store.keys());

    // The daily upload volume and the concurrent-upload limit. The window is a UTC
    // calendar day and the harness clock follows real time: keep the steps below inside
    // one day (otherwise this check depends on the hour the suite runs at).
    let now = e.h.now();
    let midnight = (now + chrono::Duration::days(1))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    if midnight - now < chrono::Duration::hours(1) {
        e.h.clock
            .advance(midnight - now + chrono::Duration::minutes(1));
        lifecycle::run_once(&e.h.state).await.unwrap();
    }
    e.policy(
        &c,
        json!({ "dailyUploadBytes": 1500, "maxConcurrentUploads": 2 }),
    )
    .await;
    let a = e.intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": 700, "sha256": sha256_hex(b"a"), "mediaType": "image/png" })).await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    let b = e.intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": 700, "sha256": sha256_hex(b"b"), "mediaType": "image/png" })).await;
    assert_eq!(b.status, StatusCode::CREATED, "{}", b.text);
    let x = e.intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": 10, "sha256": sha256_hex(b"c"), "mediaType": "image/png" })).await;
    assert_eq!(x.reason(), "CONCURRENT_UPLOADS", "{}", x.text);
    e.h.clock.advance(chrono::Duration::minutes(16));
    lifecycle::run_once(&e.h.state).await.unwrap();
    let y = e.intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": 700, "sha256": sha256_hex(b"d"), "mediaType": "image/png" })).await;
    assert_eq!(y.reason(), "DAILY_UPLOAD_VOLUME", "{}", y.text);
}

#[tokio::test]
async fn legal_holds_preserve_objects_and_are_administration_only() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let alice = people[0];
    let f = e
        .managed_upload(
            &c,
            &conv,
            alice,
            "IMAGE",
            "image/png",
            "evidence.png",
            &png(300),
        )
        .await;
    assert_eq!(f.json["state"], "READY");

    // Administration only: an acting principal is refused; scope required.
    let r =
        e.h.post(
            &c,
            Some(alice),
            "/v1/messaging/legal-holds",
            json!({ "scopeKind": "ENVIRONMENT", "reasonCategory": "LITIGATION" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let weak = e.h.credential(&c, &["messages.read"]);
    let r =
        e.h.post(
            &weak,
            None,
            "/v1/messaging/legal-holds",
            json!({ "scopeKind": "ENVIRONMENT", "reasonCategory": "LITIGATION" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let hold = e
        .h
        .post(&c, None, "/v1/messaging/legal-holds", json!({ "scopeKind": "CONVERSATION", "conversationId": conv["conversationId"], "reasonCategory": "LITIGATION", "caseReference": "CASE-2026-114" }))
        .await;
    assert_eq!(hold.status, StatusCode::CREATED, "{}", hold.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/LegalHold",
        &hold.json,
    );
    assert_eq!(hold.json["decryptionCapability"], "NONE");
    let hid = hold.json["holdId"].as_str().unwrap().to_string();
    // Another tenant can neither see nor release it.
    let other = e.h.customer(ALL_SCOPES);
    assert_eq!(
        e.h.get(&other, None, &format!("/v1/messaging/legal-holds/{hid}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let list = e.h.get(&c, None, "/v1/messaging/legal-holds").await;
    assert_eq!(
        list.json["holds"].as_array().unwrap().len(),
        1,
        "{}",
        list.text
    );

    // The abandoned upload is deleted logically, but its object is HELD, not removed.
    e.h.clock.advance(chrono::Duration::hours(25));
    let report = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(
        (report.abandoned, report.held, report.deleted_objects),
        (1, 1, 0)
    );
    assert_eq!(e.store.len(), 1, "held object preserved");
    assert_eq!(
        e.h.scalar("SELECT count(*) FROM messaging_object_deletions WHERE status = 'HELD'")
            .await,
        1
    );

    // Released (audited): the next pass deletes it.
    let r =
        e.h.post(
            &c,
            None,
            &format!("/v1/messaging/legal-holds/{hid}/release"),
            json!({ "reason": "CASE_CLOSED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "RELEASED");
    e.h.clock.advance(chrono::Duration::hours(2));
    let report = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(report.deleted_objects, 1);
    assert!(e.store.is_empty());
    let audits = e
        .h
        .scalar(&format!(
            "SELECT count(*) FROM messaging_audit WHERE environment_id = '{}' AND action IN ('LEGAL_HOLD_CREATED', 'LEGAL_HOLD_RELEASED')",
            c.context.scope.environment_id
        ))
        .await;
    assert_eq!(audits, 2);
}

#[tokio::test]
async fn attachments_fail_closed_without_storage_or_policy() {
    // No object storage configured: no intent is issued.
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({ "attachments": { "enabled": true } }))
        .await;
    let (conv, _a, _b) = h.e2ee_direct(&c).await;
    let r = h
        .post(
            &c,
            Some(_a.principal),
            &format!("/v1/conversations/{}/attachments", cid(&conv)),
            json!({ "kind": "FILE", "size": 10, "sha256": sha256_hex(b"x") }),
        )
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("FEATURE_DISABLED", "ATTACHMENT_STORAGE_NOT_CONFIGURED"),
        "{}",
        r.text
    );

    // Storage configured but attachments disabled by policy (the default).
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    let (conv, a, _b) = e.h.e2ee_direct(&c).await;
    let r = e
        .intent(&c, &conv, a.principal, json!({ "kind": "FILE", "size": 10, "sha256": sha256_hex(b"x"), "deviceId": a.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 }))
        .await;
    assert_eq!(
        (r.code(), r.reason()),
        ("FEATURE_DISABLED", "ATTACHMENTS_DISABLED"),
        "{}",
        r.text
    );
    // Storage unavailable during finalization: a clear error, nothing becomes READY.
    e.policy(&c, json!({})).await;
    let r = e
        .intent(&c, &conv, a.principal, json!({ "kind": "FILE", "size": 10, "sha256": sha256_hex(b"0123456789"), "deviceId": a.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    e.store
        .upload_signed(
            &write_of(&r.json["upload"]),
            e.h.now(),
            b"0123456789".to_vec(),
        )
        .unwrap();
    e.store.set_available(false);
    let f = e
        .finalize(&c, &conv, a.principal, &r.json["attachmentId"])
        .await;
    assert_eq!(f.code(), "STORAGE_UNAVAILABLE", "{}", f.text);
    e.store.set_available(true);
    let f = e
        .finalize(&c, &conv, a.principal, &r.json["attachmentId"])
        .await;
    assert_eq!(f.json["state"], "READY", "{}", f.text);
}

#[tokio::test]
async fn orphaned_objects_are_reconciled() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    // An object with no attachment record (crash between upload and record).
    let orphan = format!(
        "att/{}/{}/{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    e.store.put_raw(&orphan, b"lost".to_vec());
    let _ = c;
    let mut report = lifecycle::LifecycleReport::default();
    lifecycle::reconcile_orphans(&e.h.state, None, 100, &mut report)
        .await
        .unwrap();
    assert_eq!(report.orphans, 1);
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert!(e.store.get_raw(&orphan).is_none());
}

/// The MANAGED and E2EE flows against a REAL S3-compatible server (signed PUT checked by
/// the server: size, SHA-256 checksum and content type; signed GET with forced
/// download headers; unsigned access refused).
#[tokio::test]
async fn real_object_storage_end_to_end() {
    let Ok(endpoint) = std::env::var("OBJECT_STORAGE_TEST_ENDPOINT") else {
        assert!(
            std::env::var("OBJECT_STORAGE_TEST_REQUIRED").as_deref() != Ok("1"),
            "OBJECT_STORAGE_TEST_ENDPOINT is required"
        );
        eprintln!("SKIPPED: OBJECT_STORAGE_TEST_ENDPOINT not set");
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let store = Arc::new(
        cf_object_storage::S3ObjectStore::new(cf_object_storage::S3Config {
            endpoint,
            public_endpoint: None,
            region: var("OBJECT_STORAGE_TEST_REGION", "us-east-1"),
            bucket: format!(
                "{}-att",
                var("OBJECT_STORAGE_TEST_BUCKET", "cfmeet-object-storage-test")
            ),
            access_key: var("OBJECT_STORAGE_TEST_ACCESS_KEY", "testing"),
            secret_key: var("OBJECT_STORAGE_TEST_SECRET_KEY", "testing"),
            path_style: true,
            server_side_encryption: Some(var("OBJECT_STORAGE_TEST_SSE", "AES256"))
                .filter(|s| !s.is_empty()),
        })
        .unwrap(),
    );
    store.ensure_bucket().await.unwrap();
    let s2: Arc<dyn ObjectStore> = store.clone();
    let scanner = ScriptedScanner::new(ScanOutcome::Clean);
    let sc2 = scanner.clone();
    let Some(h) = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = sc2;
    })
    .await
    else {
        return;
    };
    // Signed URLs are checked against real time by the server.
    h.clock.set(chrono::Utc::now());
    let http = reqwest::Client::new();
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({ "attachments": { "enabled": true } }))
        .await;
    let (conv, mut alice, mut bob) = h.e2ee_direct(&c).await;
    let id = cid(&conv);
    let secret = vec![42u8; 300_000];
    let enc = attachment::encrypt_attachment(&secret, "video/mp4", "clip.mp4").unwrap();
    let intent = h
        .post(&c, Some(alice.principal), &format!("/v1/conversations/{id}/attachments"),
            json!({ "kind": "VIDEO", "size": enc.ciphertext.len(), "sha256": enc.descriptor.ciphertext_sha256, "deviceId": alice.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 }))
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    let upload = &intent.json["upload"];
    let put = |body: Vec<u8>| {
        let mut req = http.put(upload["url"].as_str().unwrap());
        for hd in upload["headers"].as_array().unwrap() {
            req = req.header(hd["name"].as_str().unwrap(), hd["value"].as_str().unwrap());
        }
        req.body(body).send()
    };
    // The real server refuses other bytes under the same signature.
    let mut forged = enc.ciphertext.clone();
    forged[1000] ^= 1;
    let r = put(forged).await.unwrap();
    assert!(
        !r.status().is_success(),
        "forged upload accepted: {}",
        r.status()
    );
    let r = put(enc.ciphertext.clone()).await.unwrap();
    assert!(
        r.status().is_success(),
        "{}: {}",
        r.status(),
        r.text().await.unwrap_or_default()
    );
    let aid = intent.json["attachmentId"].clone();
    let f = h
        .send(
            Method::POST,
            Some(&c.token),
            Some(alice.principal),
            &format!(
                "/v1/conversations/{id}/attachments/{}/finalize",
                aid.as_str().unwrap()
            ),
            Some(json!({})),
            &[],
        )
        .await;
    assert_eq!(f.json["state"], "READY", "{}", f.text);
    assert_eq!(scanner.calls(), 0);

    let descriptor = enc.descriptor.clone().for_attachment(uuid(&aid));
    let cmid = Uuid::new_v4();
    let ct = h.encrypt_for(&c, &conv, &mut alice, cmid, &b64(&descriptor.to_bytes()));
    let sent = h
        .post(&c, Some(alice.principal), &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": cmid, "deviceId": alice.device_id(), "epoch": alice.group.as_ref().unwrap().epoch(), "ciphertext": b64(&ct), "attachmentIds": [aid] }))
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    let got = h.receive(&c, &conv, &mut bob).await;
    let received = AttachmentDescriptor::from_bytes(&unb64(&json!(got.last().unwrap().1))).unwrap();
    let g = h
        .post(
            &c,
            Some(bob.principal),
            &format!(
                "/v1/conversations/{id}/attachments/{}/download-grants",
                aid.as_str().unwrap()
            ),
            json!({}),
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);
    let url = g.json["url"].as_str().unwrap();
    let resp = http.get(url).send().await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    assert!(resp.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    let bytes = resp.bytes().await.unwrap().to_vec();
    assert_eq!(
        attachment::decrypt_attachment(&received, &bytes).unwrap(),
        secret
    );
    // Without the signature the private bucket refuses.
    let unsigned = url.split('?').next().unwrap();
    assert!(!http
        .get(unsigned)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
}

/// A real self-hosted clamd (`MESSAGING_TEST_CLAMAV_ADDRESS`) detects the EICAR test file
/// and passes clean text.
#[tokio::test]
async fn real_clamav_detects_eicar() {
    use messaging::attachments::scanner::ClamAvScanner;
    let Ok(address) = std::env::var("MESSAGING_TEST_CLAMAV_ADDRESS") else {
        assert!(
            std::env::var("MESSAGING_CLAMAV_REQUIRED").as_deref() != Ok("1"),
            "MESSAGING_TEST_CLAMAV_ADDRESS is required"
        );
        eprintln!("SKIPPED: MESSAGING_TEST_CLAMAV_ADDRESS not set");
        return;
    };
    let scanner = Arc::new(ClamAvScanner::new(
        address,
        std::time::Duration::from_secs(30),
    ));
    // The standard antivirus test file (harmless by design), assembled at run time.
    let eicar = [
        "X5O!P%@AP[4\\PZX54(P^)7CC)7}",
        "$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*",
    ]
    .concat();
    match scanner.scan(eicar.as_bytes()).await {
        ScanOutcome::Infected(name) => {
            assert!(name.to_ascii_lowercase().contains("eicar"), "{name}")
        }
        other => panic!("EICAR not detected: {other:?}"),
    }
    assert_eq!(
        scanner.scan(b"quarterly report, nothing to see").await,
        ScanOutcome::Clean
    );
    // And through the service: an EICAR upload is quarantined.
    let store = Arc::new(MemoryObjectStore::new());
    let (s2, sc2): (Arc<dyn ObjectStore>, Arc<dyn AttachmentScanner>) =
        (store.clone(), scanner.clone());
    let Some(h) = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = sc2;
    })
    .await
    else {
        return;
    };
    let e = Env {
        h,
        store,
        scanner: ScriptedScanner::new(ScanOutcome::Clean),
    };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let f = e
        .managed_upload(
            &c,
            &conv,
            people[0],
            "DOCUMENT",
            "text/plain",
            "eicar.txt",
            eicar.as_bytes(),
        )
        .await;
    assert_eq!(f.json["state"], "QUARANTINED", "{}", f.text);
    assert_eq!(f.json["scan"]["engine"], "clamav");
}

/// Races on finalization: many concurrent finalizes of one upload, and finalizes racing
/// the intent-expiry worker. Each attachment reaches exactly ONE outcome; its quota is
/// committed or released exactly once; usage is metered once; a deletion is queued only
/// for the expired one.
#[tokio::test]
async fn concurrent_finalization_and_expiry_settle_exactly_once() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({ "maxConcurrentUploads": 50 })).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let alice = people[0];
    let env_id = c.context.scope.environment_id;

    // 1. 16 concurrent finalizes of the same upload.
    let bytes = png(4000);
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": bytes.len(), "sha256": sha256_hex(&bytes), "mediaType": "image/png", "fileName": "a.png" }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["attachmentId"].clone();
    e.store
        .upload_signed(&write_of(&r.json["upload"]), e.h.now(), bytes.clone())
        .unwrap();
    let results =
        futures_util::future::join_all((0..16).map(|_| e.finalize(&c, &conv, alice, &id))).await;
    for r in &results {
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["state"], "READY", "{}", r.text);
    }
    assert_eq!(e.usage(&c).await, (bytes.len() as i64, 0), "committed once");
    let aid = id.as_str().unwrap();
    assert_eq!(
        e.h.scalar(&format!("SELECT count(*) FROM messaging_usage_records WHERE environment_id = '{env_id}' AND subject_id = '{aid}'")).await,
        1,
        "metered once"
    );
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_object_deletions WHERE attachment_id = '{aid}'"
        ))
        .await,
        0
    );

    // 2. Finalizes racing the expiry worker once the intent's validity has passed.
    let late = png(3000);
    let r = e
        .intent(&c, &conv, alice, json!({ "kind": "IMAGE", "size": late.len(), "sha256": sha256_hex(&late), "mediaType": "image/png", "fileName": "b.png" }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let late_id = r.json["attachmentId"].clone();
    e.store
        .upload_signed(&write_of(&r.json["upload"]), e.h.now(), late.clone())
        .unwrap();
    e.h.clock.advance(chrono::Duration::minutes(16));
    let finalizes =
        futures_util::future::join_all((0..8).map(|_| e.finalize(&c, &conv, alice, &late_id)));
    let workers = futures_util::future::join_all((0..4).map(|_| lifecycle::run_once(&e.h.state)));
    let (finalized, swept) = tokio::join!(finalizes, workers);
    for r in &finalized {
        assert!(
            r.reason() == "INTENT_EXPIRED" || r.json["state"] == "EXPIRED",
            "an expired intent never becomes READY: {}",
            r.text
        );
    }
    let expired_by_workers: usize = swept
        .iter()
        .map(|r| r.as_ref().unwrap().expired_intents)
        .sum();
    assert!(expired_by_workers <= 1, "{expired_by_workers}");
    let lid = late_id.as_str().unwrap();
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_attachments WHERE id = '{lid}' AND state = 'EXPIRED'"
        ))
        .await,
        1
    );
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_object_deletions WHERE attachment_id = '{lid}'"
        ))
        .await,
        1,
        "exactly one deletion queued"
    );
    // The first attachment's commitment is untouched; the expired reservation is released
    // exactly once (never negative).
    assert_eq!(e.usage(&c).await, (bytes.len() as i64, 0));
    println!("EVIDENCE finalize_race concurrent_finalizes=16 finalize_vs_expiry=8x4 outcomes=1");
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

/// A real PNG (gradient) with an optional private tEXt chunk before IEND.
fn real_png(w: u32, h: u32, text: Option<&str>) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    if let Some(t) = text {
        let iend = png.len() - 12;
        let mut data = b"Comment\0".to_vec();
        data.extend_from_slice(t.as_bytes());
        let c = chunk(b"tEXt", &data);
        png.splice(iend..iend, c);
    }
    png
}

#[tokio::test]
async fn managed_images_get_safe_thumbnails_and_e2ee_attachments_never_do() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 3).await;
    let (alice, bob, outsider_conv_member) = (people[0], people[1], people[2]);
    let id = cid(&conv);
    let secret_note = "GPS 51.5007,-0.1246 private-comment-7f3a";
    let png = real_png(1200, 800, Some(secret_note));
    let jpeg = {
        let img = image::RgbImage::from_fn(640, 480, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, 9])
        });
        let mut b = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Jpeg)
            .unwrap();
        b
    };
    // A decompression-bomb header: a valid 1x1 PNG whose IHDR claims 9000 x 9000.
    let bomb = {
        let mut b = real_png(1, 1, None);
        let mut ihdr = b[16..29].to_vec();
        ihdr[0..4].copy_from_slice(&9000u32.to_be_bytes());
        ihdr[4..8].copy_from_slice(&9000u32.to_be_bytes());
        b[16..29].copy_from_slice(&ihdr);
        let mut crc_input = b"IHDR".to_vec();
        crc_input.extend_from_slice(&ihdr);
        b[29..33].copy_from_slice(&crc32(&crc_input).to_be_bytes());
        b
    };
    let mut ids = Vec::new();
    for (name, media, bytes) in [
        ("photo.png", "image/png", png.clone()),
        ("photo.jpg", "image/jpeg", jpeg),
        ("bomb.png", "image/png", bomb),
        ("broken.png", "image/png", self::png(3000)),
    ] {
        let r = e
            .managed_upload(&c, &conv, alice, "IMAGE", media, name, &bytes)
            .await;
        assert_eq!(r.json["state"], "READY", "{}", r.text);
        ids.push(r.json["attachmentId"].as_str().unwrap().to_string());
    }
    let sent =
        e.h.post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "pictures", "attachmentIds": ids }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);

    // An E2EE attachment in another conversation: ciphertext, never processed.
    let (e2ee_conv, alice_d, _bob_d) = e.h.e2ee_direct(&c).await;
    let enc =
        attachment::encrypt_attachment(&real_png(64, 64, None), "image/png", "x.png").unwrap();
    let intent = e
        .intent(&c, &e2ee_conv, alice_d.principal, json!({ "kind": "IMAGE", "size": enc.ciphertext.len(), "sha256": enc.descriptor.ciphertext_sha256, "deviceId": alice_d.device_id(), "cryptoFormat": attachment::ATTACHMENT_FORMAT_V1 }))
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    e.store
        .upload_signed(
            &write_of(&intent.json["upload"]),
            e.h.now(),
            enc.ciphertext.clone(),
        )
        .unwrap();
    let e2ee_id = intent.json["attachmentId"].clone();
    let r = e
        .finalize(&c, &e2ee_conv, alice_d.principal, &e2ee_id)
        .await;
    assert_eq!(r.json["state"], "READY", "{}", r.text);

    let report = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(
        (report.derivatives_ready, report.derivatives_failed),
        // Thumbnails: 2 made, 2 refused. Inspections: the 3 decodable headers (the bomb's
        // header is read, never its pixels), the corrupt file refused.
        (5, 3),
        "{report:?}"
    );
    let e2ee = e2ee_id.as_str().unwrap();
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_attachment_derivatives WHERE attachment_id = '{e2ee}'"
        ))
        .await,
        0,
        "E2EE ciphertext is never processed"
    );
    assert!(
        e.h.sql(&format!(
            "INSERT INTO messaging_attachment_derivatives (id, attachment_id, environment_id, security_profile, kind, status, next_attempt_at, created_at) \
             SELECT gen_random_uuid(), id, environment_id, 'MANAGED', 'THUMBNAIL', 'PENDING', now(), now() FROM messaging_attachments WHERE id = '{e2ee}'"
        ))
        .await
        .is_err(),
        "the database refuses a derivative of a PRIVATE_E2EE attachment"
    );

    // Another member sees the derivative metadata and gets a thumbnail grant.
    let meta =
        e.h.get(
            &c,
            Some(bob),
            &format!("/v1/conversations/{id}/attachments/{}", ids[0]),
        )
        .await;
    assert_eq!(meta.status, StatusCode::OK, "{}", meta.text);
    conforms(
        "messaging/common/v1.schema.json#/$defs/Attachment",
        &meta.json,
    );
    let of = |kind: &str| {
        meta.json["derivatives"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["kind"] == kind)
            .cloned()
            .unwrap_or_else(|| panic!("no {kind}: {}", meta.text))
    };
    let inspected = of("INSPECTION");
    assert_eq!(inspected["status"], "READY", "{inspected}");
    assert_eq!(inspected["metadata"]["container"], "PNG");
    assert_eq!(
        (
            inspected["metadata"]["width"].as_i64(),
            inspected["metadata"]["height"].as_i64()
        ),
        (Some(1200), Some(800))
    );
    let d = &of("THUMBNAIL");
    assert_eq!(
        (d["kind"].as_str(), d["status"].as_str()),
        (Some("THUMBNAIL"), Some("READY")),
        "{d}"
    );
    assert_eq!(
        (d["width"].as_i64(), d["height"].as_i64()),
        (Some(320), Some(213))
    );
    assert_eq!(
        (d["sourceWidth"].as_i64(), d["sourceHeight"].as_i64()),
        (Some(1200), Some(800))
    );
    let g =
        e.h.post(
            &c,
            Some(bob),
            &format!(
                "/v1/conversations/{id}/attachments/{}/thumbnail-grants",
                ids[0]
            ),
            json!({}),
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);
    assert_eq!(g.json["mediaType"], "image/png");
    // The thumbnail is a re-encoded PNG: bounded, decodable, and the private text is gone.
    let key = thumbnail_object(&e, &ids[0]).await;
    let thumb = e.store.get(&key, 10 << 20).await.unwrap().unwrap();
    let decoded = image::load_from_memory(&thumb).unwrap();
    assert!(decoded.width() <= 320 && decoded.height() <= 320);
    assert!(!thumb
        .windows(secret_note.len())
        .any(|w| w == secret_note.as_bytes()));
    assert!(
        png.windows(secret_note.len())
            .any(|w| w == secret_note.as_bytes()),
        "the original had it"
    );
    // Failures are truthful and never produce an object.
    let failed: Vec<String> = ids[2..].iter().map(|i| format!("'{i}'")).collect();
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_attachment_derivatives WHERE attachment_id IN ({}) AND kind = 'THUMBNAIL' AND status = 'FAILED' \
               AND object_key IS NULL AND error_code IN ('TOO_MANY_PIXELS', 'UNDECODABLE')",
            failed.join(",")
        ))
        .await,
        2
    );
    // Inspection reads the bomb's header (9000 x 9000, never its pixels) and refuses the
    // corrupt file.
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_attachment_derivatives WHERE attachment_id = '{}' AND kind = 'INSPECTION' AND status = 'READY' \
               AND (metadata->>'width')::int = 9000 AND (metadata->>'height')::int = 9000",
            ids[2]
        ))
        .await,
        1
    );
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_attachment_derivatives WHERE attachment_id = '{}' AND kind = 'INSPECTION' AND status = 'FAILED' \
               AND metadata IS NULL AND error_code = 'UNDECODABLE'",
            ids[3]
        ))
        .await,
        1
    );
    let r =
        e.h.post(
            &c,
            Some(bob),
            &format!(
                "/v1/conversations/{id}/attachments/{}/thumbnail-grants",
                ids[2]
            ),
            json!({}),
        )
        .await;
    assert_eq!(r.reason(), "THUMBNAIL_NOT_READY", "{}", r.text);
    // A non-member gets nothing.
    let stranger = e.h.principal(&c);
    let r =
        e.h.post(
            &c,
            Some(stranger),
            &format!(
                "/v1/conversations/{id}/attachments/{}/thumbnail-grants",
                ids[0]
            ),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let _ = outsider_conv_member;
    // READY thumbnail bytes are part of the *same* per-environment committed quota
    // as original (including E2EE ciphertext) attachment bytes. INSPECTION has no
    // stored object and contributes nothing. The database trigger must enforce this
    // once, including when a worker retries or inspects the same derivative twice.
    let expected_originals = e.h.scalar(&format!(
        "SELECT COALESCE(sum(committed_bytes), 0)::bigint FROM messaging_attachments WHERE environment_id = '{}'",
        c.context.scope.environment_id
    )).await;
    let expected_thumbnails = e.h.scalar(&format!(
        "SELECT COALESCE(sum(bytes), 0)::bigint FROM messaging_attachment_derivatives WHERE environment_id = '{}' AND kind = 'THUMBNAIL' AND status = 'READY'",
        c.context.scope.environment_id
    )).await;
    assert!(expected_thumbnails > 0, "two real thumbnails were rendered");
    let committed_before_delete = e.usage(&c).await.0;
    assert_eq!(
        committed_before_delete,
        expected_originals + expected_thumbnails,
        "generated object bytes must count against the environment storage quota"
    );
    // An upload that would have fit if thumbnail bytes were ignored must now be
    // refused at the same per-environment storage-budget boundary.
    e.policy(&c, json!({ "storageBudgetBytes": committed_before_delete }))
        .await;
    let extra = real_png(1, 1, None);
    let denied = e
        .intent(
            &c,
            &conv,
            alice,
            json!({
                "kind": "IMAGE",
                "size": extra.len(),
                "sha256": sha256_hex(&extra),
                "mediaType": "image/png"
            }),
        )
        .await;
    assert_eq!(denied.reason(), "STORAGE_BUDGET", "{}", denied.text);
    // Running the worker again must not double-count the same thumbnail.
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(e.usage(&c).await.0, committed_before_delete);

    // Deleting the attachment queues its thumbnail object for deletion too.
    let row = messaging::attachments::load(
        &e.h.state.db,
        Uuid::parse_str(&id).unwrap(),
        Uuid::parse_str(&ids[0]).unwrap(),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        lifecycle::delete_attachment(&e.h.state.db, &row, "TEST_DELETE", e.h.now())
            .await
            .unwrap()
    );
    let generated_bytes = d["size"].as_i64().expect("measured thumbnail bytes");
    // Both objects still exist until the deletion worker removes them: their bytes stay
    // charged, now through their deletion requests.
    assert_eq!(e.usage(&c).await.0, committed_before_delete);
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_object_deletions WHERE object_key = '{key}' AND charged_bytes = {generated_bytes}"
        ))
        .await,
        1
    );
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(
        e.usage(&c).await.0,
        committed_before_delete - row.committed_bytes - generated_bytes,
        "both original and derivative quota usage must be released exactly once"
    );
    assert_no_drift(&e).await;
    assert!(
        e.store.get(&key, 10 << 20).await.unwrap().is_none(),
        "the thumbnail object is gone"
    );
    println!("EVIDENCE derivatives thumbnails=2 refused=2 e2ee_processed=0");
    // §118 / Appendix D: the transitions above recorded their domain events, and every
    // event conforms to the published event contract.
    let types = e.h.conforming_event_types().await;
    for t in [
        "messaging.attachment.ready",
        "messaging.attachment.derivative-ready",
    ] {
        assert!(types.contains(t), "{t} not recorded: {types:?}");
    }
}

fn test_wav(ms_len: u32) -> Vec<u8> {
    let data = vec![0u8; (32 * ms_len) as usize];
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&[1, 0, 1, 0]);
    v.extend_from_slice(&16_000u32.to_le_bytes());
    v.extend_from_slice(&32_000u32.to_le_bytes());
    v.extend_from_slice(&[2, 0, 16, 0]);
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(&data);
    v
}

fn test_mp4(seconds: u32) -> Vec<u8> {
    let bx = |kind: &[u8; 4], body: &[u8]| {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    };
    let mut mvhd = vec![0u8; 100];
    mvhd[12..16].copy_from_slice(&600u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&(600 * seconds).to_be_bytes());
    let mut tkhd = vec![0u8; 84];
    tkhd[76..80].copy_from_slice(&(1280u32 << 16).to_be_bytes());
    tkhd[80..84].copy_from_slice(&(720u32 << 16).to_be_bytes());
    let mut hdlr = vec![0u8; 24];
    hdlr[8..12].copy_from_slice(b"vide");
    let trak = bx(
        b"trak",
        &[bx(b"tkhd", &tkhd), bx(b"mdia", &bx(b"hdlr", &hdlr))].concat(),
    );
    [
        bx(b"ftyp", b"isom\0\0\x02\0isomiso2mp41"),
        bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat()),
        bx(b"mdat", &[9u8; 4096]),
    ]
    .concat()
}

#[tokio::test]
async fn managed_voice_notes_and_videos_are_measured_not_trusted() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 2).await;
    let alice = people[0];
    let mut ids = Vec::new();
    for (kind, media, name, bytes, declared) in [
        (
            "VOICE_NOTE",
            "audio/wav",
            "memo.wav",
            test_wav(4_000),
            4_100,
        ),
        ("VIDEO", "video/mp4", "clip.mp4", test_mp4(12), 60_000),
    ] {
        let r = e
            .intent(
                &c,
                &conv,
                alice,
                json!({ "kind": kind, "size": bytes.len(), "sha256": sha256_hex(&bytes),
                "mediaType": media, "fileName": name, "durationMs": declared }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        assert_eq!(
            r.json["media"]["durationVerified"], false,
            "the declaration alone is never 'verified'"
        );
        e.store
            .upload_signed(&write_of(&r.json["upload"]), e.h.now(), bytes.clone())
            .unwrap();
        let f = e.finalize(&c, &conv, alice, &r.json["attachmentId"]).await;
        assert_eq!(f.json["state"], "READY", "{}", f.text);
        ids.push(r.json["attachmentId"].as_str().unwrap().to_string());
    }
    let report = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(
        (report.derivatives_ready, report.derivatives_failed),
        (2, 0),
        "{report:?}"
    );
    let id = cid(&conv);
    let mut found = Vec::new();
    for aid in &ids {
        let m =
            e.h.get(
                &c,
                Some(alice),
                &format!("/v1/conversations/{id}/attachments/{aid}"),
            )
            .await;
        assert_eq!(m.status, StatusCode::OK, "{}", m.text);
        conforms("messaging/common/v1.schema.json#/$defs/Attachment", &m.json);
        found.push(
            m.json["derivatives"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["kind"] == "INSPECTION")
                .cloned()
                .unwrap(),
        );
    }
    let voice = found[0].clone();
    assert_eq!(voice["status"], "READY", "{voice}");
    assert_eq!(voice["metadata"]["container"], "WAV");
    assert_eq!(voice["metadata"]["durationMs"], 4_000);
    assert_eq!(
        voice["metadata"]["durationMatchesDeclared"], true,
        "4.1 s declared, 4.0 s measured"
    );
    let video = found[1].clone();
    assert_eq!(video["metadata"]["container"], "MP4");
    assert_eq!(video["metadata"]["durationMs"], 12_000);
    assert_eq!(
        (
            video["metadata"]["width"].as_i64(),
            video["metadata"]["height"].as_i64()
        ),
        (Some(1280), Some(720))
    );
    assert_eq!(video["metadata"]["declaredDurationMs"], 60_000);
    assert_eq!(
        video["metadata"]["durationMatchesDeclared"], false,
        "a false declaration is exposed, not believed"
    );
    // Inspection holds no object; deleting the attachment clears its metadata.
    let row = messaging::attachments::load(
        &e.h.state.db,
        Uuid::parse_str(&id).unwrap(),
        Uuid::parse_str(&ids[1]).unwrap(),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        lifecycle::delete_attachment(&e.h.state.db, &row, "TEST_DELETE", e.h.now())
            .await
            .unwrap()
    );
    assert_eq!(
        e.h.scalar(&format!(
            "SELECT count(*) FROM messaging_attachment_derivatives WHERE attachment_id = '{}' AND (metadata IS NOT NULL OR status <> 'DELETED')",
            ids[1]
        ))
        .await,
        0
    );
    println!("EVIDENCE inspection wav_ms=4000 mp4_ms=12000 declared_mismatch_flagged=1");
}

// ---------------------------------------------------------------------------
// Derivative storage accounting (ADR 0054, migrations 18 and 19)
// ---------------------------------------------------------------------------

use messaging::attachments::{derivatives, quota};
use sea_orm::ConnectionTrait;

/// How the derivative-write wrapper answers `put` of a `drv/` object.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DerivativeWrites {
    Normal,
    /// The write fails and stores nothing.
    Fail,
    /// The write stores the object, then the answer is lost (an unknown outcome).
    WriteThenFail,
}

/// The in-memory store with failure injection for derivative writes only (originals,
/// reads and deletions keep working).
struct FlakyDerivatives {
    inner: Arc<MemoryObjectStore>,
    mode: Mutex<DerivativeWrites>,
}

#[async_trait::async_trait]
impl ObjectStore for FlakyDerivatives {
    fn backend(&self) -> &'static str {
        "memory-flaky-derivatives"
    }
    async fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> cf_object_storage::StorageResult<cf_object_storage::StoredObject> {
        let mode = *self.mode.lock().unwrap();
        if key.starts_with(derivatives::OBJECT_PREFIX) {
            match mode {
                DerivativeWrites::Normal => {}
                DerivativeWrites::Fail => return Err(cf_object_storage::StorageError::Unavailable),
                DerivativeWrites::WriteThenFail => {
                    self.inner.put(key, bytes, content_type).await?;
                    return Err(cf_object_storage::StorageError::Unavailable);
                }
            }
        }
        self.inner.put(key, bytes, content_type).await
    }
    async fn head(
        &self,
        key: &str,
    ) -> cf_object_storage::StorageResult<Option<cf_object_storage::ObjectMeta>> {
        self.inner.head(key).await
    }
    async fn get(
        &self,
        key: &str,
        max_bytes: usize,
    ) -> cf_object_storage::StorageResult<Option<Vec<u8>>> {
        self.inner.get(key, max_bytes).await
    }
    async fn delete(&self, key: &str) -> cf_object_storage::StorageResult<()> {
        self.inner.delete(key).await
    }
    fn signed_read(
        &self,
        key: &str,
        ttl: std::time::Duration,
        now: chrono::DateTime<chrono::Utc>,
    ) -> cf_object_storage::StorageResult<cf_object_storage::SignedRead> {
        self.inner.signed_read(key, ttl, now)
    }
    fn signed_read_as(
        &self,
        key: &str,
        ttl: std::time::Duration,
        now: chrono::DateTime<chrono::Utc>,
        overrides: &cf_object_storage::ResponseOverrides,
    ) -> cf_object_storage::StorageResult<cf_object_storage::SignedRead> {
        self.inner.signed_read_as(key, ttl, now, overrides)
    }
    fn signed_write(
        &self,
        key: &str,
        ttl: std::time::Duration,
        now: chrono::DateTime<chrono::Utc>,
        size: u64,
        sha256_hex: &str,
        content_type: &str,
    ) -> cf_object_storage::StorageResult<SignedWrite> {
        self.inner
            .signed_write(key, ttl, now, size, sha256_hex, content_type)
    }
    async fn stored_sha256(&self, key: &str) -> cf_object_storage::StorageResult<Option<String>> {
        self.inner.stored_sha256(key).await
    }
    async fn list(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        max: usize,
    ) -> cf_object_storage::StorageResult<Vec<cf_object_storage::ListedObject>> {
        self.inner.list(prefix, start_after, max).await
    }
}

async fn flaky_env() -> Option<(Env, Arc<FlakyDerivatives>)> {
    let store = Arc::new(MemoryObjectStore::new());
    let flaky = Arc::new(FlakyDerivatives {
        inner: store.clone(),
        mode: Mutex::new(DerivativeWrites::Normal),
    });
    let scanner = ScriptedScanner::new(ScanOutcome::Clean);
    let (f2, sc2) = (flaky.clone(), scanner.clone());
    let h = Harness::build(move |s| {
        s.storage = f2;
        s.scanner = sc2;
    })
    .await?;
    Some((Env { h, store, scanner }, flaky))
}

async fn text(e: &Env, sql: &str) -> Option<String> {
    e.h.state
        .db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap()
        .and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
}

/// The object key of an attachment's READY thumbnail.
async fn thumbnail_object(e: &Env, attachment_id: &str) -> String {
    text(
        e,
        &format!(
            "SELECT object_key FROM messaging_attachment_derivatives WHERE attachment_id = '{attachment_id}' \
               AND kind = 'THUMBNAIL' AND status = 'READY'"
        ),
    )
    .await
    .unwrap_or_else(|| panic!("no READY thumbnail of {attachment_id}"))
}

async fn thumbnail_status(e: &Env, attachment_id: &str) -> (String, Option<String>, i64) {
    let status = text(e, &format!("SELECT status FROM messaging_attachment_derivatives WHERE attachment_id = '{attachment_id}' AND kind = 'THUMBNAIL'")).await.unwrap();
    let code = text(e, &format!("SELECT error_code FROM messaging_attachment_derivatives WHERE attachment_id = '{attachment_id}' AND kind = 'THUMBNAIL'")).await;
    let reserved = e.h.scalar(&format!("SELECT reserved_bytes FROM messaging_attachment_derivatives WHERE attachment_id = '{attachment_id}' AND kind = 'THUMBNAIL'")).await;
    (status, code, reserved)
}

/// Every environment's recorded usage equals what its rows add up to.
async fn assert_no_drift(e: &Env) {
    let drifted = quota::audit_all(&e.h.state.db).await.unwrap();
    assert!(drifted.is_empty(), "storage accounting drift: {drifted:?}");
}

fn derivative_objects(e: &Env) -> Vec<String> {
    e.store
        .keys()
        .into_iter()
        .filter(|k| k.starts_with(derivatives::OBJECT_PREFIX))
        .collect()
}

/// A distinct real PNG and the size of the thumbnail the server will make of it.
fn image_and_thumbnail(seed: u8) -> (Vec<u8>, i64) {
    let img = image::RgbImage::from_fn(900, 600, |x, y| {
        image::Rgb([(x % 256) as u8 ^ seed, (y % 256) as u8, seed])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let t = derivatives::render_thumbnail(&png, "image/png").unwrap();
    (png, t.png.len() as i64)
}

impl Env {
    async fn upload_image(&self, c: &Customer, conv: &Value, who: Uuid, png: &[u8]) -> String {
        let r = self
            .managed_upload(c, conv, who, "IMAGE", "image/png", "photo.png", png)
            .await;
        assert_eq!(r.json["state"], "READY", "{}", r.text);
        r.json["attachmentId"].as_str().unwrap().to_string()
    }
}

/// Part K gate 6 / ADR 0054: a thumbnail is reserved against the budget, under the usage
/// row lock, before its object is written. When the budget has no room the original stays
/// READY and usable, the thumbnail is QUOTA_DEFERRED with no object (never stored
/// unaccounted), and it is produced once room exists. Concurrent workers cannot overshoot,
/// and another environment's budget is independent.
#[tokio::test]
async fn thumbnails_reserve_storage_before_writing_and_defer_over_budget() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 2).await;
    let (a, ta) = image_and_thumbnail(1);
    let (b, tb) = image_and_thumbnail(2);
    let ida = e.upload_image(&c, &conv, people[0], &a).await;
    let idb = e.upload_image(&c, &conv, people[0], &b).await;
    let originals = (a.len() + b.len()) as i64;
    assert_eq!(e.usage(&c).await, (originals, 0));
    // Room for exactly one of the two thumbnails.
    let budget = originals + ta.max(tb) + ta.min(tb) - 1;
    e.policy(&c, json!({ "storageBudgetBytes": budget })).await;
    // Another environment, unlimited, with its own image.
    let other = e.h.customer(ALL_SCOPES);
    e.policy(&other, json!({})).await;
    let (oconv, opeople) = e.managed_group(&other, 1).await;
    let (o, to) = image_and_thumbnail(3);
    let ido = e.upload_image(&other, &oconv, opeople[0], &o).await;

    // Eight workers race over the queue (FOR UPDATE SKIP LOCKED leases + the usage lock).
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let s = e.h.state.clone();
            tokio::spawn(async move { derivatives::process_due(&s, 20).await.unwrap() })
        })
        .collect();
    let mut deferred = 0;
    for w in workers {
        deferred += w.await.unwrap().deferred;
    }
    assert_eq!(deferred, 1, "exactly one thumbnail does not fit");
    let (sa, sb) = (
        thumbnail_status(&e, &ida).await,
        thumbnail_status(&e, &idb).await,
    );
    let (ready_id, deferred_id, ready_size) = match (sa.0.as_str(), sb.0.as_str()) {
        ("READY", "QUOTA_DEFERRED") => (&ida, &idb, ta),
        ("QUOTA_DEFERRED", "READY") => (&idb, &ida, tb),
        other => panic!("unexpected thumbnail states {other:?}"),
    };
    let d = thumbnail_status(&e, deferred_id).await;
    assert_eq!((d.1.as_deref(), d.2), (Some("STORAGE_BUDGET"), 0));
    assert_eq!(
        e.h.scalar(&format!("SELECT count(*) FROM messaging_attachment_derivatives WHERE attachment_id = '{deferred_id}' AND kind = 'THUMBNAIL' AND object_key IS NULL AND pending_object_key IS NULL")).await,
        1,
        "a deferred thumbnail has no object"
    );
    // One stored derivative object for this environment, and it is the READY one.
    let env_a = c.context.scope.environment_id.to_string();
    let mine: Vec<String> = derivative_objects(&e)
        .into_iter()
        .filter(|k| k.contains(&env_a))
        .collect();
    assert_eq!(mine, vec![thumbnail_object(&e, ready_id).await]);
    assert_eq!(e.usage(&c).await, (originals + ready_size, 0));
    assert!(e.usage(&c).await.0 <= budget, "never over the budget");
    // The actual stored object size is what is charged.
    assert_eq!(
        e.store.get_raw(&mine[0]).unwrap().len() as i64,
        ready_size,
        "charged bytes are the persisted object's size"
    );
    // The other environment's thumbnail was made regardless.
    assert_eq!(thumbnail_status(&e, &ido).await.0, "READY");
    assert_eq!(e.usage(&other).await, (o.len() as i64 + to, 0));

    // The original of the deferred thumbnail is untouched and downloadable; the API tells
    // the truth about the derivative.
    // (Not yet sent in a message: only the uploader can read it.)
    let meta =
        e.h.get(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{}/attachments/{deferred_id}", cid(&conv)),
        )
        .await;
    assert_eq!(meta.status, StatusCode::OK, "{}", meta.text);
    assert_eq!(meta.json["state"], "READY");
    conforms(
        "messaging/common/v1.schema.json#/$defs/Attachment",
        &meta.json,
    );
    let t = meta.json["derivatives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "THUMBNAIL")
        .cloned()
        .unwrap();
    assert_eq!(
        (t["status"].as_str(), t["errorCode"].as_str()),
        (Some("QUOTA_DEFERRED"), Some("STORAGE_BUDGET"))
    );
    let g = e.grant(&c, &conv, people[0], &json!(deferred_id)).await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.text);

    // Re-running before the retry time changes nothing (no object, no charge).
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(thumbnail_status(&e, deferred_id).await.0, "QUOTA_DEFERRED");
    assert_eq!(e.usage(&c).await, (originals + ready_size, 0));
    // Even when the retry is due, a still-full budget defers again.
    e.h.clock
        .advance(derivatives::QUOTA_RETRY + chrono::Duration::seconds(1));
    let r = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(r.derivatives_deferred, 1, "{r:?}");
    assert_eq!(e.usage(&c).await, (originals + ready_size, 0));

    // An authorized budget change requeues it at once.
    e.policy(&c, json!({ "storageBudgetBytes": originals + ta + tb }))
        .await;
    let r = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(r.derivatives_ready, 1, "{r:?}");
    assert_eq!(thumbnail_status(&e, deferred_id).await.0, "READY");
    assert_eq!(e.usage(&c).await, (originals + ta + tb, 0));
    assert_no_drift(&e).await;
    println!("EVIDENCE derivative-quota deferral: originals={originals} thumbnails={ta}+{tb} budget={budget} deferred=1 workers=8 drift=0");
}

/// A worker that wrote its thumbnail object and died before the database acknowledged it
/// (lease expired, reservation still recorded). The next attempt retires that object to
/// the deletion queue (still charged), writes its own, and the environment ends charged
/// exactly once for the stored thumbnail.
#[tokio::test]
async fn a_crash_between_thumbnail_upload_and_acknowledgement_is_charged_once() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (png, t) = image_and_thumbnail(7);
    let id = e.upload_image(&c, &conv, people[0], &png).await;
    let s = png.len() as i64;
    // Enqueue only (no budget for decoding yet: the pre-check defers without rendering).
    e.policy(&c, json!({ "storageBudgetBytes": s })).await;
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(thumbnail_status(&e, &id).await.0, "QUOTA_DEFERRED");
    assert!(
        derivative_objects(&e).is_empty(),
        "nothing rendered or stored"
    );
    e.policy(&c, json!({})).await;
    // The crashed attempt: reserved, its object written, its lease expired.
    let env = c.context.scope.environment_id;
    let stale = derivatives::attempt_key(env, Uuid::parse_str(&id).unwrap(), Uuid::new_v4());
    e.store.put_raw(&stale, vec![7u8; t as usize]);
    e.h.sql(&format!(
        "UPDATE messaging_attachment_derivatives SET status = 'PENDING', error_code = NULL, reserved_bytes = {t}, \
           pending_object_key = '{stale}', lease_token = gen_random_uuid(), lease_expires_at = now() - interval '1 hour', \
           next_attempt_at = now() - interval '1 hour' \
         WHERE attachment_id = '{id}' AND kind = 'THUMBNAIL'"
    ))
    .await
    .unwrap();
    assert_eq!(
        e.usage(&c).await,
        (s, t),
        "the crashed attempt is reserved, not free"
    );
    assert_no_drift(&e).await;
    e.h.clock
        .set(chrono::Utc::now() + chrono::Duration::minutes(5));

    let r = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!((r.derivatives_ready, r.deleted_objects), (1, 1), "{r:?}");
    let key = thumbnail_object(&e, &id).await;
    assert_ne!(
        key, stale,
        "the retry never reuses the crashed attempt's key"
    );
    assert!(
        e.store.get_raw(&stale).is_none(),
        "the crashed attempt's object is deleted"
    );
    assert_eq!(derivative_objects(&e), vec![key.clone()]);
    assert_eq!(e.store.get_raw(&key).unwrap().len() as i64, t);
    assert_eq!(e.usage(&c).await, (s + t, 0), "charged exactly once");
    assert_eq!(
        e.h.scalar(&format!("SELECT count(*) FROM messaging_object_deletions WHERE object_key = '{stale}' AND status = 'DONE' AND charged_bytes = {t}")).await,
        1
    );
    assert_no_drift(&e).await;
}

/// Storage outages while writing a thumbnail: a write that fails, and a write that stored
/// the object but lost its answer. Every attempt's object is retired with its charge;
/// after the final failure no derivative object remains, the original is untouched and
/// the environment is charged for the original only.
#[tokio::test]
async fn failed_thumbnail_writes_release_their_reservation_and_leave_no_object() {
    let Some((e, flaky)) = flaky_env().await else {
        return;
    };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (png, t) = image_and_thumbnail(9);
    let id = e.upload_image(&c, &conv, people[0], &png).await;
    let s = png.len() as i64;
    for mode in [
        DerivativeWrites::WriteThenFail,
        DerivativeWrites::Fail,
        DerivativeWrites::WriteThenFail,
    ] {
        *flaky.mode.lock().unwrap() = mode;
        lifecycle::run_once(&e.h.state).await.unwrap();
        let st = thumbnail_status(&e, &id).await;
        if st.0 == "PENDING" {
            // Between attempts the outcome is unknown: the attempt stays reserved.
            assert_eq!(st.2, t, "{st:?}");
            assert_eq!(e.usage(&c).await.1, t);
        }
        assert_no_drift(&e).await;
        e.h.clock.advance(chrono::Duration::minutes(2));
    }
    let st = thumbnail_status(&e, &id).await;
    assert_eq!(
        (st.0.as_str(), st.1.as_deref(), st.2),
        ("FAILED", Some("STORAGE_UNAVAILABLE"), 0)
    );
    *flaky.mode.lock().unwrap() = DerivativeWrites::Normal;
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert!(
        derivative_objects(&e).is_empty(),
        "{:?}",
        derivative_objects(&e)
    );
    assert_eq!(e.usage(&c).await, (s, 0));
    let a =
        e.h.get(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{}/attachments/{id}", cid(&conv)),
        )
        .await;
    assert_eq!(
        a.json["state"], "READY",
        "the original survives every derivative failure"
    );
    assert_no_drift(&e).await;
}

/// Legal holds preserve objects and their accounting: a deleted attachment whose objects
/// are HELD stays charged (original and thumbnail) until the hold is released and the
/// objects are really removed.
#[tokio::test]
async fn held_derivatives_keep_their_storage_charge_until_really_deleted() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (png, t) = image_and_thumbnail(11);
    let id = e.upload_image(&c, &conv, people[0], &png).await;
    let s = png.len() as i64;
    lifecycle::run_once(&e.h.state).await.unwrap();
    let thumb = thumbnail_object(&e, &id).await;
    assert_eq!(e.usage(&c).await, (s + t, 0));
    let hold = e.h.post(&c, None, "/v1/messaging/legal-holds", json!({ "scopeKind": "CONVERSATION", "conversationId": conv["conversationId"], "reasonCategory": "LITIGATION" })).await;
    assert_eq!(hold.status, StatusCode::CREATED, "{}", hold.text);
    let row = messaging::attachments::load(
        &e.h.state.db,
        Uuid::parse_str(&cid(&conv)).unwrap(),
        Uuid::parse_str(&id).unwrap(),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        lifecycle::delete_attachment(&e.h.state.db, &row, "TEST_DELETE", e.h.now())
            .await
            .unwrap()
    );
    let r = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!((r.held, r.deleted_objects), (2, 0), "{r:?}");
    assert!(e.store.get_raw(&thumb).is_some() && e.store.get_raw(&row.object_key).is_some());
    assert_eq!(
        e.usage(&c).await,
        (s + t, 0),
        "held objects keep their accounting"
    );
    assert_no_drift(&e).await;
    let rel =
        e.h.post(
            &c,
            None,
            &format!(
                "/v1/messaging/legal-holds/{}/release",
                hold.json["holdId"].as_str().unwrap()
            ),
            json!({ "reason": "CASE_CLOSED" }),
        )
        .await;
    assert_eq!(rel.status, StatusCode::OK, "{}", rel.text);
    e.h.clock.advance(chrono::Duration::hours(2));
    let r = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(r.deleted_objects, 2, "{r:?}");
    assert!(e.store.is_empty(), "{:?}", e.store.keys());
    assert_eq!(e.usage(&c).await, (0, 0));
    assert_no_drift(&e).await;
}

/// Retention purges an expired message's attachment and its thumbnail; both charges are
/// released once the objects are gone.
#[tokio::test]
async fn retention_purge_releases_derivative_storage() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.h.set_policy(&c, json!({
        "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "attachments": { "enabled": true },
        "retention": { "messageDays": 30 },
    })).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (png, t) = image_and_thumbnail(13);
    let id = e.upload_image(&c, &conv, people[0], &png).await;
    let sent =
        e.h.post(
            &c,
            Some(people[0]),
            &format!("/v1/conversations/{}/messages", cid(&conv)),
            json!({ "clientMessageId": Uuid::new_v4(), "text": "photo", "attachmentIds": [id] }),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.text);
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!(e.usage(&c).await, (png.len() as i64 + t, 0));
    e.h.clock.advance(chrono::Duration::days(31));
    let r = messaging::compliance::run_once(&e.h.state).await.unwrap();
    assert_eq!(r.attachments_deleted, 1, "{r:?}");
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert!(e.store.is_empty(), "{:?}", e.store.keys());
    assert_eq!(e.usage(&c).await, (0, 0));
    assert_no_drift(&e).await;
    // §118 / Appendix D: the transitions above recorded their domain events, and every
    // event conforms to the published event contract.
    let types = e.h.conforming_event_types().await;
    for t in ["messaging.attachment.deleted", "messaging.message.purged"] {
        assert!(types.contains(t), "{t} not recorded: {types:?}");
    }
}

/// A regenerated (replaced) thumbnail: the old object stays charged until deleted, the new
/// one is reserved and charged on its own — usage never under-counts what is stored and
/// ends at exactly the new object's size.
#[tokio::test]
async fn a_replaced_thumbnail_adjusts_usage_exactly() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (png, t) = image_and_thumbnail(17);
    let id = e.upload_image(&c, &conv, people[0], &png).await;
    let s = png.len() as i64;
    lifecycle::run_once(&e.h.state).await.unwrap();
    let old = thumbnail_object(&e, &id).await;
    assert!(derivatives::regenerate_thumbnail(
        &e.h.state.db,
        Uuid::parse_str(&id).unwrap(),
        e.h.now()
    )
    .await
    .unwrap());
    assert_eq!(
        e.usage(&c).await,
        (s + t, 0),
        "the old object is still charged"
    );
    assert_no_drift(&e).await;
    let r = lifecycle::run_once(&e.h.state).await.unwrap();
    assert_eq!((r.derivatives_ready, r.deleted_objects), (1, 1), "{r:?}");
    let new = thumbnail_object(&e, &id).await;
    assert_ne!(old, new);
    assert!(e.store.get_raw(&old).is_none());
    assert_eq!(derivative_objects(&e), vec![new]);
    assert_eq!(e.usage(&c).await, (s + t, 0));
    // Nothing to regenerate for a missing READY thumbnail.
    assert!(
        !derivatives::regenerate_thumbnail(&e.h.state.db, Uuid::new_v4(), e.h.now())
            .await
            .unwrap()
    );
    assert_no_drift(&e).await;
}

/// Orphaned derivative objects (no row references them) are found and deleted; live
/// thumbnails and objects already queued are left alone.
#[tokio::test]
async fn orphaned_derivative_objects_are_reconciled() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (png, t) = image_and_thumbnail(19);
    let id = e.upload_image(&c, &conv, people[0], &png).await;
    lifecycle::run_once(&e.h.state).await.unwrap();
    let live = thumbnail_object(&e, &id).await;
    let env = c.context.scope.environment_id;
    let orphan = derivatives::attempt_key(env, Uuid::parse_str(&id).unwrap(), Uuid::new_v4());
    e.store.put_raw(&orphan, vec![1u8; 64]);
    let mut report = lifecycle::LifecycleReport::default();
    assert!(
        lifecycle::reconcile_derivative_orphans(&e.h.state, None, 100, &mut report)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(report.orphans, 1);
    lifecycle::run_once(&e.h.state).await.unwrap();
    assert!(e.store.get_raw(&orphan).is_none());
    assert!(
        e.store.get_raw(&live).is_some(),
        "the live thumbnail is kept"
    );
    assert_eq!(
        e.usage(&c).await,
        (png.len() as i64 + t, 0),
        "orphans were never charged"
    );
    assert_no_drift(&e).await;
}

/// Existing derivatives (migration path): rolled back to the schema before derivative
/// accounting, the environment is charged for originals only; upgrading backfills every
/// READY thumbnail exactly once, and migration 19 alone neither charges nor releases.
#[tokio::test]
async fn existing_thumbnails_are_backfilled_once_by_the_upgrade() {
    let Some(e) = env().await else { return };
    let c = e.h.customer(ALL_SCOPES);
    e.policy(&c, json!({})).await;
    let (conv, people) = e.managed_group(&c, 1).await;
    let (a, ta) = image_and_thumbnail(23);
    let (b, tb) = image_and_thumbnail(29);
    e.upload_image(&c, &conv, people[0], &a).await;
    e.upload_image(&c, &conv, people[0], &b).await;
    lifecycle::run_once(&e.h.state).await.unwrap();
    let s = (a.len() + b.len()) as i64;
    assert_eq!(e.usage(&c).await, (s + ta + tb, 0));
    use messaging_migration::MigratorTrait;
    let conn = &e.h.db.conn;
    // Back to before migration 18 (migrations 21, 20, 19 and 18 rolled back).
    messaging_migration::Migrator::down(conn, Some(4))
        .await
        .unwrap();
    assert_eq!(
        e.usage(&c).await,
        (s, 0),
        "the pre-ADR-0054 schema charged originals only"
    );
    messaging_migration::Migrator::up(conn, Some(1))
        .await
        .unwrap();
    assert_eq!(
        e.usage(&c).await,
        (s + ta + tb, 0),
        "migration 18 backfills each READY thumbnail once"
    );
    messaging_migration::Migrator::up(conn, None).await.unwrap();
    assert_eq!(
        e.usage(&c).await,
        (s + ta + tb, 0),
        "migration 19 changes no existing charge"
    );
    assert!(messaging_migration::Migrator::get_pending_migrations(conn)
        .await
        .unwrap()
        .is_empty());
    // 21, 20 and 19 down / up (migrations 20 and 21 do not touch storage accounting).
    messaging_migration::Migrator::down(conn, Some(3))
        .await
        .unwrap();
    messaging_migration::Migrator::up(conn, None).await.unwrap();
    assert_eq!(
        e.usage(&c).await,
        (s + ta + tb, 0),
        "19 down / up is neutral"
    );
    assert_no_drift(&e).await;
}

/// Gate 6 on a real S3-compatible server (MinIO in CI): the charged thumbnail bytes equal
/// the size the store reports for the persisted object, deletion releases them only once
/// both objects are gone from the bucket, and an orphaned derivative object in the bucket
/// is found and removed.
#[tokio::test]
async fn real_object_storage_thumbnail_accounting() {
    let Ok(endpoint) = std::env::var("OBJECT_STORAGE_TEST_ENDPOINT") else {
        assert!(
            std::env::var("OBJECT_STORAGE_TEST_REQUIRED").as_deref() != Ok("1"),
            "OBJECT_STORAGE_TEST_ENDPOINT is required"
        );
        eprintln!("SKIPPED: OBJECT_STORAGE_TEST_ENDPOINT not set");
        return;
    };
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let store = Arc::new(
        cf_object_storage::S3ObjectStore::new(cf_object_storage::S3Config {
            endpoint,
            public_endpoint: None,
            region: var("OBJECT_STORAGE_TEST_REGION", "us-east-1"),
            bucket: format!(
                "{}-drv",
                var("OBJECT_STORAGE_TEST_BUCKET", "cfmeet-object-storage-test")
            ),
            access_key: var("OBJECT_STORAGE_TEST_ACCESS_KEY", "testing"),
            secret_key: var("OBJECT_STORAGE_TEST_SECRET_KEY", "testing"),
            path_style: true,
            server_side_encryption: Some(var("OBJECT_STORAGE_TEST_SSE", "AES256"))
                .filter(|s| !s.is_empty()),
        })
        .unwrap(),
    );
    store.ensure_bucket().await.unwrap();
    let s2: Arc<dyn ObjectStore> = store.clone();
    let Some(h) = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = ScriptedScanner::new(ScanOutcome::Clean);
    })
    .await
    else {
        return;
    };
    h.clock.set(chrono::Utc::now());
    let http = reqwest::Client::new();
    let c = h.customer(ALL_SCOPES);
    h.set_policy(&c, json!({
        "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } },
        "attachments": { "enabled": true },
    }))
    .await;
    let alice = h.principal(&c);
    let conv = h
        .create_conversation(
            &c,
            alice,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(conv.status, StatusCode::CREATED, "{}", conv.text);
    let id = cid(&conv.json);
    let (png, _) = image_and_thumbnail(31);
    let intent = h
        .post(&c, Some(alice), &format!("/v1/conversations/{id}/attachments"),
            json!({ "kind": "IMAGE", "size": png.len(), "sha256": sha256_hex(&png), "mediaType": "image/png", "fileName": "real.png" }))
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    let upload = &intent.json["upload"];
    let mut req = http.put(upload["url"].as_str().unwrap());
    for hd in upload["headers"].as_array().unwrap() {
        req = req.header(hd["name"].as_str().unwrap(), hd["value"].as_str().unwrap());
    }
    let r = req.body(png.clone()).send().await.unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let aid = intent.json["attachmentId"].as_str().unwrap().to_string();
    let f = h
        .post(
            &c,
            Some(alice),
            &format!("/v1/conversations/{id}/attachments/{aid}/finalize"),
            json!({}),
        )
        .await;
    assert_eq!(f.json["state"], "READY", "{}", f.text);
    let r = lifecycle::run_once(&h.state).await.unwrap();
    assert_eq!(r.derivatives_ready, 2, "thumbnail + inspection: {r:?}");

    let usage = |h: &Harness| {
        let env = c.context.scope.environment_id;
        let db = h.state.db.clone();
        async move {
            db.query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT committed_bytes, reserved_bytes FROM messaging_storage_usage WHERE environment_id = '{env}'"),
            ))
            .await
            .unwrap()
            .map(|r| (r.try_get_by_index::<i64>(0).unwrap(), r.try_get_by_index::<i64>(1).unwrap()))
            .unwrap()
        }
    };
    let key: String = h
        .state
        .db
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT object_key FROM messaging_attachment_derivatives WHERE attachment_id = '{aid}' AND kind = 'THUMBNAIL' AND status = 'READY'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let stored = store
        .head(&key)
        .await
        .unwrap()
        .expect("thumbnail in the bucket");
    let original = store
        .head(&messaging::attachments::object_key(
            c.context.scope.environment_id,
            Uuid::parse_str(&id).unwrap(),
            Uuid::parse_str(&aid).unwrap(),
        ))
        .await
        .unwrap()
        .expect("original in the bucket");
    assert_eq!(original.size, png.len() as u64);
    assert_eq!(
        usage(&h).await,
        ((original.size + stored.size) as i64, 0),
        "charged bytes are the sizes the bucket reports"
    );
    // A stray derivative object (as a lost answer would leave) is found and removed.
    let stray = derivatives::attempt_key(
        c.context.scope.environment_id,
        Uuid::parse_str(&aid).unwrap(),
        Uuid::new_v4(),
    );
    store
        .put(&stray, vec![0u8; 128], "image/png")
        .await
        .unwrap();
    h.clock.advance(chrono::Duration::hours(2));
    let mut report = lifecycle::LifecycleReport::default();
    let mut cursor = None;
    loop {
        cursor = lifecycle::reconcile_derivative_orphans(&h.state, cursor, 1000, &mut report)
            .await
            .unwrap();
        if cursor.is_none() {
            break;
        }
    }
    assert!(report.orphans >= 1, "{report:?}");
    // Delete the attachment: both objects leave the bucket, then the charge is released.
    let row = messaging::attachments::load(
        &h.state.db,
        Uuid::parse_str(&id).unwrap(),
        Uuid::parse_str(&aid).unwrap(),
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        lifecycle::delete_attachment(&h.state.db, &row, "TEST_DELETE", h.now())
            .await
            .unwrap()
    );
    assert_eq!(
        usage(&h).await.0,
        (original.size + stored.size) as i64,
        "still stored, still charged"
    );
    lifecycle::run_once(&h.state).await.unwrap();
    assert!(store.head(&key).await.unwrap().is_none());
    assert!(store.head(&stray).await.unwrap().is_none());
    assert!(store.head(&row.object_key).await.unwrap().is_none());
    assert_eq!(usage(&h).await, (0, 0));
    assert!(quota::audit_all(&h.state.db).await.unwrap().is_empty());
    println!(
        "EVIDENCE real-s3 thumbnail accounting: original={} thumbnail={} released=both",
        original.size, stored.size
    );
}
