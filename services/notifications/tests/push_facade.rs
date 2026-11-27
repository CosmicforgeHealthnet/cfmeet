//! Roadmap N9 (Phase 4.1) against a real database: the D-1b copy and registration facade
//! of Messaging push registrations (I13), message wake-ups delivered by the service to the
//! copied endpoints with Messaging's metadata-only payload, Web Push signed with the
//! shared VAPID key at a local fake push service (RFC 8030 / 8291 / 8292 checked
//! byte-wise), and the call fast lane's recipient filter and delivery options (I3).

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::Method;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use cf_notification_client::Producer;
use cf_push_transport::{
    adapters::{vapid_public_key, PushAdapters},
    config::{PushAdapterConfig, WebPushConfig},
    PushProvider, SendOptions,
};
use chrono::Utc;
use common::*;
use notifications::authorities::{AccountDevice, EntitledPrincipal, WakeupAuthorization};
use serde_json::{json, Value};
use uuid::Uuid;

const SYNC: &str = "/internal/v1/push/registrations/sync";
/// Messaging's CF Meet personal scope in these tests.
const TENANT: &str = "7a000000-0000-4000-8000-000000000001";
const APP: &str = "7a000000-0000-4000-8000-000000000002";
const ENVIRONMENT: &str = "7a000000-0000-4000-8000-000000000003";

fn item(
    device: Uuid,
    account: Option<&str>,
    platform: &str,
    handle: Option<&str>,
    enabled: bool,
) -> Value {
    let mut v = json!({
        "tenantId": TENANT, "applicationId": APP, "environmentId": ENVIRONMENT,
        "principalId": "7a000000-0000-4000-8000-0000000000aa",
        "deviceId": device, "platform": platform, "enabled": enabled,
    });
    if let Some(a) = account {
        v["accountRef"] = json!(a);
    }
    if let Some(h) = handle {
        v["handle"] = json!(h);
    }
    v
}

async fn sync(h: &Harness, items: Vec<Value>) -> Vec<Value> {
    let messaging = h.peers.messaging.clone();
    let r = h
        .signed(
            &messaging,
            Method::POST,
            SYNC,
            Some(json!({ "registrations": items })),
            &[],
        )
        .await;
    r.expect(200)["results"].as_array().unwrap().clone()
}

/// Captures every log line of the test process. Installed once as the global subscriber: a
/// thread-local `set_default` subscriber races with tracing's per-callsite interest cache
/// while other tests of this binary run in parallel, and could miss lines.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn global() -> &'static Captured {
        static LOGS: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
        LOGS.get_or_init(|| {
            let logs = Captured::default();
            let writer = logs.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("the only global subscriber of this test binary");
            logs
        })
    }
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    /// Everything logged since `from` (by any test of this binary: a stricter secret check).
    fn text_since(&self, from: usize) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()[from..]).to_string()
    }
}

#[tokio::test]
async fn the_copy_is_idempotent_and_never_echoes_logs_or_stores_handles_in_clear() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let logs = Captured::global();
    let from = logs.len();
    let (d1, d2) = (Uuid::new_v4(), Uuid::new_v4());
    let secret_a = "local-handle-SECRET-aaaa-0001";
    let secret_b = "local-handle-SECRET-bbbb-0002";
    let batch = vec![
        item(d1, Some(&alice()), "LOCAL_TEST", Some(secret_a), true),
        item(d2, Some(&bob()), "LOCAL_TEST", Some(secret_b), true),
    ];
    let first = sync(&h, batch.clone()).await;
    assert_eq!(first[0]["outcome"], "CREATED");
    assert_eq!(first[1]["outcome"], "CREATED");
    let target = first[0]["pushTargetRef"].clone();
    assert!(target.is_string());
    // Replaying the copy changes nothing (verification counts stay equal).
    let again = sync(&h, batch).await;
    assert_eq!(again[0]["outcome"], "UNCHANGED");
    assert_eq!(again[1]["outcome"], "UNCHANGED");
    assert_eq!(again[0]["pushTargetRef"], target);
    assert_eq!(
        h.count(&format!("SELECT count(*)::bigint FROM notif_push_endpoints WHERE environment_id = '{ENVIRONMENT}' AND enabled"))
            .await,
        2
    );
    // Owned by the CF Meet account, keyed by Messaging's device, principal recorded.
    assert_eq!(
        h.text(&format!(
            "SELECT account_ref FROM notif_push_endpoints WHERE account_device_ref = '{d1}'"
        ))
        .await
        .as_deref(),
        Some(alice().as_str())
    );
    // Never echoed, never stored in clear, never logged; the seal opens only for its row.
    for r in first.iter().chain(&again) {
        assert!(!r.to_string().contains("SECRET"), "{r}");
    }
    let sealed = h
        .text(&format!(
            "SELECT handle_sealed FROM notif_push_endpoints WHERE account_device_ref = '{d1}'"
        ))
        .await
        .unwrap();
    let id = target.as_str().unwrap();
    assert_eq!(
        h.state.keys.open(&sealed, &format!("push:{id}")).unwrap(),
        secret_a
    );
    assert!(h.state.keys.open(&sealed, "push:other").is_err());
    // A refused caller and a refused item log their refusal, never the handle.
    let calling = h.peers.calling.clone();
    h.signed(
        &calling,
        Method::POST,
        SYNC,
        Some(json!({ "registrations": [item(d1, Some(&alice()), "LOCAL_TEST", Some(secret_a), true)] })),
        &[],
    )
    .await
    .expect_error(403, "FORBIDDEN");
    sync(
        &h,
        vec![item(d1, Some(&alice()), "WEB_PUSH", Some(secret_a), true)],
    )
    .await;
    let text = logs.text_since(from);
    assert!(text.contains("internal permission denied"), "{text}");
    assert!(
        !text.contains("SECRET"),
        "a handle reached the logs: {}",
        text.lines()
            .filter(|l| l.contains("SECRET"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Checked after the capture (this query itself carries the marker).
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE handle_sealed LIKE '%SECRET%' OR handle_digest LIKE '%SECRET%'").await, 0);
}

#[tokio::test]
async fn the_facade_follows_changes_and_removals_and_defers_to_account_devices() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let device = Uuid::new_v4();
    let created = sync(
        &h,
        vec![item(
            device,
            Some(&alice()),
            "LOCAL_TEST",
            Some("handle-v1"),
            true,
        )],
    )
    .await;
    let target = created[0]["pushTargetRef"].clone();
    // A changed handle updates the same endpoint (pushTargetRef stable).
    let changed = sync(
        &h,
        vec![item(
            device,
            Some(&alice()),
            "LOCAL_TEST",
            Some("handle-v2"),
            true,
        )],
    )
    .await;
    assert_eq!(changed[0]["outcome"], "UPDATED");
    assert_eq!(changed[0]["pushTargetRef"], target);
    // Removal (unregister, device revoked, no longer first-party) is idempotent.
    let removed = sync(&h, vec![item(device, None, "LOCAL_TEST", None, false)]).await;
    assert_eq!(removed[0]["outcome"], "REMOVED");
    let again = sync(&h, vec![item(device, None, "LOCAL_TEST", None, false)]).await;
    assert_eq!(again[0]["outcome"], "ABSENT");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints")
            .await,
        0
    );
    // Only CF Meet accounts; nothing but known platforms; refusals never echo the handle.
    let bad = sync(
        &h,
        vec![
            item(
                device,
                Some("platform-principal:x"),
                "LOCAL_TEST",
                Some("h-SECRET"),
                true,
            ),
            item(device, None, "LOCAL_TEST", Some("h-SECRET"), true),
            item(device, Some(&alice()), "SMS", Some("h-SECRET"), true),
            item(
                device,
                Some(&alice()),
                "WEB_PUSH",
                Some("not-json-SECRET"),
                true,
            ),
        ],
    )
    .await;
    for (r, reason) in bad
        .iter()
        .zip(["ACCOUNT_REF", "ACCOUNT_REF", "PLATFORM", "HANDLE"])
    {
        assert_eq!(r["outcome"], "INVALID", "{r}");
        assert_eq!(r["reason"], reason, "{r}");
        assert!(!r.to_string().contains("SECRET"));
    }
    // The account-device registration (U8) of the same handle is authoritative.
    let account_device = Uuid::new_v4();
    h.owners
        .contacts
        .lock()
        .unwrap()
        .get_mut(&alice())
        .unwrap()
        .devices
        .push(AccountDevice {
            account_device_ref: account_device.to_string(),
            state: "ACTIVE".into(),
        });
    sync(
        &h,
        vec![item(
            device,
            Some(&alice()),
            "LOCAL_TEST",
            Some("shared-handle"),
            true,
        )],
    )
    .await;
    h.user(
        &alice(),
        "PUT",
        &format!("/devices/{account_device}/push"),
        None,
        Some(json!({"platform": "LOCAL_TEST", "handle": "shared-handle"})),
    )
    .await
    .expect(200);
    // U8 took the handle over (one endpoint per handle) …
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints")
            .await,
        1
    );
    // … and the facade never takes it back, for the same account or another one.
    let same = sync(
        &h,
        vec![item(
            device,
            Some(&alice()),
            "LOCAL_TEST",
            Some("shared-handle"),
            true,
        )],
    )
    .await;
    assert_eq!(same[0]["outcome"], "SUPERSEDED");
    let other = sync(
        &h,
        vec![item(
            Uuid::new_v4(),
            Some(&bob()),
            "LOCAL_TEST",
            Some("shared-handle"),
            true,
        )],
    )
    .await;
    assert_eq!(other[0]["outcome"], "HANDLE_IN_USE");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE environment_id = '00000000-0000-0000-0000-000000000000'")
            .await,
        1
    );
    // Only Messaging holds the permission; batches are bounded.
    let calling = h.peers.calling.clone();
    h.signed(
        &calling,
        Method::POST,
        SYNC,
        Some(json!({ "registrations": [item(device, None, "LOCAL_TEST", None, false)] })),
        &[],
    )
    .await
    .expect_error(403, "FORBIDDEN");
    let messaging = h.peers.messaging.clone();
    h.signed(
        &messaging,
        Method::POST,
        SYNC,
        Some(json!({ "registrations": [] })),
        &[],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    // With the CF Meet first-party scope configured, other scopes are not mirrored.
    h.state.first_party_scope = Some((
        Uuid::parse_str(TENANT).unwrap(),
        Uuid::parse_str(APP).unwrap(),
        Uuid::new_v4(),
    ));
    h.router = notifications::http::app(h.state.clone());
    let out = sync(
        &h,
        vec![item(
            Uuid::new_v4(),
            Some(&alice()),
            "LOCAL_TEST",
            Some("h3"),
            true,
        )],
    )
    .await;
    assert_eq!(out[0]["outcome"], "OUT_OF_SCOPE");
}

fn wakeup_event(conversation: Uuid) -> Value {
    envelope(
        Producer::Messaging,
        "messaging.message.created",
        &[alice()],
        &[
            ("conversationId", json!(conversation.to_string())),
            ("conversationKind", json!("DIRECT")),
            ("eventSequence", json!(12)),
            ("securityProfile", json!("PRIVATE_E2EE")),
            ("sequence", json!(7)),
        ],
        None,
    )
}

fn entitle(h: &Harness, conversation: Uuid) {
    h.owners.wakeups.lock().unwrap().insert(
        conversation,
        WakeupAuthorization {
            conversation_active: true,
            security_profile: "PRIVATE_E2EE".into(),
            conversation_kind: "DIRECT".into(),
            entitled: vec![EntitledPrincipal {
                principal: "7a000000-0000-4000-8000-0000000000aa".into(),
                account_ref: Some(alice()),
                mentions_recipient: false,
            }],
        },
    );
}

#[tokio::test]
async fn copied_endpoints_receive_service_wakeups_with_the_legacy_payload() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let device = Uuid::new_v4();
    sync(
        &h,
        vec![item(
            device,
            Some(&alice()),
            "LOCAL_TEST",
            Some("copied-handle"),
            true,
        )],
    )
    .await;
    let conversation = Uuid::new_v4();
    entitle(&h, conversation);
    h.set_category("MESSAGE_WAKEUPS", "ACTIVE").await;
    let messaging = h.peers.messaging.clone();
    let r = h.ingest(&messaging, vec![wakeup_event(conversation)]).await;
    assert_eq!(r.expect(200)["results"][0]["outcome"], "ACCEPTED");
    h.process_all().await;
    let t = h.push_worker().tick(Utc::now()).await.unwrap();
    assert_eq!(t.sent, 1, "{t:?}");
    let sent = h.push.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "copied-handle");
    // Exactly Messaging's legacy job payload: 5 metadata keys, never content.
    assert_eq!(
        sent[0].1,
        json!({ "v": 1, "type": "message", "conversationId": conversation.to_string(), "eventSequence": 12, "sequence": 7 })
    );
    assert_eq!(
        cf_push_transport::adapters::metadata(&sent[0].1)
            .unwrap()
            .len(),
        5
    );
    // A removed registration gets nothing.
    sync(&h, vec![item(device, None, "LOCAL_TEST", None, false)]).await;
    let r = h.ingest(&messaging, vec![wakeup_event(conversation)]).await;
    r.expect(200);
    h.process_all().await;
    assert_eq!(h.push_worker().tick(Utc::now()).await.unwrap().sent, 0);
    assert_eq!(h.push.sent.lock().unwrap().len(), 1);
}

// --------------------------------------------------------------------- Web Push, end to end

/// One request received by the fake push service.
#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

async fn fake_push_service() -> (String, Arc<Mutex<Vec<Received>>>) {
    use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
    let seen: Arc<Mutex<Vec<Received>>> = Arc::default();
    async fn accept(
        State(seen): State<Arc<Mutex<Vec<Received>>>>,
        uri: axum::http::Uri,
        headers: axum::http::HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        seen.lock().unwrap().push(Received {
            path: uri.path().to_string(),
            headers,
            body: body.to_vec(),
        });
        StatusCode::CREATED
    }
    let app = Router::new()
        .route("/push/:id", post(accept))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let authority = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (authority, seen)
}

/// A browser: its subscription keys and the handle it registers.
struct Browser {
    secret: p256::SecretKey,
    auth: [u8; 16],
    handle: String,
}

impl Browser {
    fn new(endpoint: &str) -> Self {
        let secret = p256::SecretKey::from_slice(&rand::random::<[u8; 32]>()).unwrap();
        let auth: [u8; 16] = rand::random();
        let p256dh = B64URL.encode(
            p256::elliptic_curve::sec1::ToSec1Point::to_sec1_point(
                secret.public_key().as_affine(),
                false,
            )
            .as_bytes(),
        );
        let handle = json!({ "endpoint": endpoint, "keys": { "p256dh": p256dh, "auth": B64URL.encode(auth) } })
            .to_string();
        Self {
            secret,
            auth,
            handle,
        }
    }

    /// RFC 8291 §3.4 / RFC 8188 decryption of one `aes128gcm` record, as a browser does.
    fn decrypt(&self, body: &[u8]) -> Value {
        use hmac::{Hmac, Mac};
        use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_128_GCM};
        let hmac = |key: &[u8], parts: &[&[u8]]| -> [u8; 32] {
            let mut m = <Hmac<sha2::Sha256> as Mac>::new_from_slice(key).unwrap();
            for p in parts {
                m.update(p);
            }
            m.finalize().into_bytes().into()
        };
        let salt = &body[..16];
        let rs = u32::from_be_bytes(body[16..20].try_into().unwrap());
        assert_eq!(rs, 4096);
        assert_eq!(body[20], 65);
        let as_public = p256::PublicKey::from_sec1_bytes(&body[21..86]).unwrap();
        let ciphertext = &body[86..];
        let ua_public = p256::elliptic_curve::sec1::ToSec1Point::to_sec1_point(
            self.secret.public_key().as_affine(),
            false,
        );
        let shared =
            p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), as_public.as_affine());
        let prk_key = hmac(&self.auth, &[&shared.raw_secret_bytes()[..]]);
        let ikm = hmac(
            &prk_key,
            &[
                b"WebPush: info\0",
                ua_public.as_bytes(),
                &body[21..86],
                &[1],
            ],
        );
        let prk = hmac(salt, &[&ikm]);
        let cek = hmac(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]]);
        let nonce = hmac(&prk, &[b"Content-Encoding: nonce\0", &[1]]);
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek[..16]).unwrap());
        let mut record = ciphertext.to_vec();
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce[..12].try_into().unwrap()),
                Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(plain.last(), Some(&2), "last-record delimiter");
        serde_json::from_slice(&plain[..plain.len() - 1]).unwrap()
    }
}

/// Verifies the RFC 8292 `vapid t=<jwt>, k=<key>` authorization against `expected_key`.
fn verify_vapid(r: &Received, expected_key: &str, audience: &str) {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    let auth = r.headers["authorization"].to_str().unwrap();
    let rest = auth.strip_prefix("vapid t=").unwrap();
    let (jwt, k) = rest.split_once(", k=").unwrap();
    assert_eq!(k, expected_key, "signed with the shared VAPID key");
    let key = VerifyingKey::from_sec1_bytes(&B64URL.decode(k).unwrap()).unwrap();
    let (input, sig) = jwt.rsplit_once('.').unwrap();
    let sig = Signature::from_slice(&B64URL.decode(sig).unwrap()).unwrap();
    key.verify(input.as_bytes(), &sig).unwrap();
    let claims: Value =
        serde_json::from_slice(&B64URL.decode(input.split('.').nth(1).unwrap()).unwrap()).unwrap();
    assert_eq!(claims["aud"], audience);
    assert_eq!(claims["sub"], "https://cfmeet.com");
    assert!(claims["exp"].as_i64().unwrap() > Utc::now().timestamp());
}

#[tokio::test]
async fn web_push_uses_messagings_vapid_key_file_at_a_local_push_service() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let (authority, seen) = fake_push_service().await;
    // The key file exactly as `activate-web-push` writes it for Messaging (openssl genpkey
    // PKCS#8 PEM, stripped, one trailing newline), mounted into the service.
    let pem = {
        use p256::pkcs8::{EncodePrivateKey, LineEnding};
        let secret = p256::SecretKey::from_slice(&rand::random::<[u8; 32]>()).unwrap();
        format!("{}\n", secret.to_pkcs8_pem(LineEnding::LF).unwrap().trim())
    };
    let dir = std::env::temp_dir().join(format!("n9-vapid-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let key_file = dir.join("web-push-vapid.pem");
    std::fs::write(&key_file, &pem).unwrap();
    let config = PushAdapterConfig {
        web: Some(WebPushConfig {
            vapid_key_file: key_file.clone(),
            subject: "https://cfmeet.com".into(),
            allowed_hosts: vec![authority.clone()],
        }),
        ..Default::default()
    };
    // Messaging and the service build the adapter the same way from the same file: the
    // application-server key browsers subscribed with does not change.
    let adapters: Arc<dyn PushProvider> =
        Arc::new(PushAdapters::build(&config, true, None).unwrap());
    let public = adapters.web_push_application_server_key().unwrap();
    assert_eq!(public, vapid_public_key(&pem).unwrap());
    std::fs::remove_dir_all(&dir).ok();
    h.state.push = Some(adapters.clone());
    h.router = notifications::http::app(h.state.clone());
    assert_eq!(
        h.raw(Method::GET, "/ready", Default::default(), None)
            .await
            .json["webPushApplicationServerKey"],
        public
    );

    // A browser subscription Messaging accepted, copied through the facade.
    let browser = Browser::new(&format!("http://{authority}/push/sub-1"));
    let device = Uuid::new_v4();
    let copied = sync(
        &h,
        vec![item(
            device,
            Some(&alice()),
            "WEB_PUSH",
            Some(&browser.handle),
            true,
        )],
    )
    .await;
    assert_eq!(copied[0]["outcome"], "CREATED", "{copied:?}");
    let target = copied[0]["pushTargetRef"].clone();

    // Message wake-up through the service (W1 → W3).
    let conversation = Uuid::new_v4();
    entitle(&h, conversation);
    h.set_category("MESSAGE_WAKEUPS", "ACTIVE").await;
    let messaging = h.peers.messaging.clone();
    h.ingest(&messaging, vec![wakeup_event(conversation)])
        .await
        .expect(200);
    h.process_all().await;
    let worker = notifications::push::PushWorker::new(
        h.state.db.clone(),
        h.state.keys.clone(),
        adapters.clone(),
        h.state.metrics.clone(),
    );
    assert_eq!(worker.tick(Utc::now()).await.unwrap().sent, 1);
    let r = seen.lock().unwrap()[0].clone();
    assert_eq!(r.path, "/push/sub-1");
    verify_vapid(&r, &public, &format!("http://{authority}"));
    // Messaging's delivery parameters: 24 h TTL, urgency high, no collapse topic.
    assert_eq!(r.headers["ttl"], "86400");
    assert_eq!(r.headers["urgency"], "high");
    assert_eq!(r.headers["content-encoding"], "aes128gcm");
    assert!(r.headers.get("topic").is_none());
    let payload = browser.decrypt(&r.body);
    assert_eq!(
        payload,
        json!({ "conversationId": conversation.to_string(), "eventSequence": "12", "sequence": "7", "type": "message", "v": "1" })
    );

    // The call fast lane to the same endpoint: short TTL, collapsing topic, metadata only.
    h.set_category("CALL_INVITATIONS", "ACTIVE").await;
    let calling = h.peers.calling.clone();
    let out = h
        .signed(
            &calling,
            Method::POST,
            "/internal/v1/push/call-invitations",
            Some(json!({
                "pushTargetRefs": [target],
                "payload": {"schema": "cfmeet.calling.push-invitation", "schemaVersion": 1, "callId": Uuid::new_v4(), "caller": {"displayName": "Bob"}},
                "expiresAt": (Utc::now() + chrono::Duration::seconds(45)).to_rfc3339(),
                "recipientAccountRef": alice(),
            })),
            &[],
        )
        .await;
    assert_eq!(out.expect(200)["results"][0]["outcome"], "SENT");
    let r = seen.lock().unwrap()[1].clone();
    verify_vapid(&r, &public, &format!("http://{authority}"));
    let ttl: u64 = r.headers["ttl"].to_str().unwrap().parse().unwrap();
    assert!((1..=45).contains(&ttl), "{ttl}");
    assert_eq!(r.headers["topic"], "cfmeet-call");
    assert_eq!(r.headers["urgency"], "high");
    let payload = browser.decrypt(&r.body);
    assert_eq!(payload, json!({ "type": "call", "v": "1" }));
    assert!(!payload.to_string().contains("Bob"));
}

#[tokio::test]
async fn the_call_fast_lane_wakes_only_the_invited_accounts_endpoints() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mine = sync(
        &h,
        vec![item(
            Uuid::new_v4(),
            Some(&alice()),
            "LOCAL_TEST",
            Some("alice-h"),
            true,
        )],
    )
    .await[0]["pushTargetRef"]
        .clone();
    let theirs = sync(
        &h,
        vec![item(
            Uuid::new_v4(),
            Some(&bob()),
            "LOCAL_TEST",
            Some("bob-h"),
            true,
        )],
    )
    .await[0]["pushTargetRef"]
        .clone();
    h.set_category("CALL_INVITATIONS", "ACTIVE").await;
    let calling = h.peers.calling.clone();
    let expires = (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
    let out = h
        .signed(
            &calling,
            Method::POST,
            "/internal/v1/push/call-invitations",
            Some(json!({
                "pushTargetRefs": [mine, theirs],
                "payload": {"v": 1},
                "expiresAt": expires,
                "recipientAccountRef": alice(),
            })),
            &[],
        )
        .await;
    let results = out.expect(200)["results"].as_array().unwrap().clone();
    assert_eq!(results[0]["outcome"], "SENT");
    assert_eq!(results[1]["outcome"], "UNKNOWN_TARGET");
    let sent = h.push.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "alice-h");
    assert_eq!(sent[0].1, json!({ "v": 1, "type": "call" }));
    let options = h.push.options.lock().unwrap().clone();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].collapse_id.as_deref(), Some("cfmeet-call"));
    assert!(options[0].ttl_seconds.unwrap() <= 30);
    assert!(!options[0].voip);
    assert_eq!(
        notifications::push::call_send_options("APNS_VOIP", Utc::now(), Utc::now()),
        SendOptions {
            ttl_seconds: Some(0),
            collapse_id: Some("cfmeet-call".into()),
            voip: true
        }
    );
    // A malformed recipient reference is refused.
    h.signed(
        &calling,
        Method::POST,
        "/internal/v1/push/call-invitations",
        Some(json!({ "pushTargetRefs": [mine], "payload": {}, "expiresAt": expires, "recipientAccountRef": "user:1" })),
        &[],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
}
