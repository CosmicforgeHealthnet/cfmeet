//! ADR 0063 (phase B5) against a real database: partner push through the Notification
//! Service, behind `NOTIFICATIONS_PARTNER_PUSH_ENABLED`.
//!
//! * N-P1 the provider vault (store / list / verify / delete; the secret is never readable
//!   back from responses, logs or the database; a seal or credential of one scope never
//!   serves another);
//! * N-P2 `platform-principal` registrations through Messaging's facade (I13);
//! * N-P3 the call fast lane: APNS_VOIP (PushKit, `<bundleId>.voip`) and FCM high-priority
//!   data with the recipient's own scope credential, never CF Meet's;
//! * N-P4 `platform-principal:<env>:<principal>` recipients of `messaging.message.created`
//!   (metadata only, collapsed per conversation);
//! * N-P5 per-scope counters and disabling of refused handles;
//! * with the flag off every route and shape behaves exactly as before.
//!
//! The provider network is the deterministic `RecordingPartnerTransport`.

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::Method;
use cf_notification_client::{
    envelope::Scope as EnvScope, platform_principal_ref, push_credential_fingerprint,
    seal_push_credential, EnvelopeBuilder, Producer,
};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const SYNC: &str = "/internal/v1/push/registrations/sync";
const CALL: &str = "/internal/v1/push/call-invitations";

#[derive(Clone, Copy)]
struct S {
    t: Uuid,
    a: Uuid,
    e: Uuid,
}

fn scope(n: u8) -> S {
    let id = |k: u8| {
        Uuid::parse_str(&format!(
            "{n:02x}{k:02x}0000-0000-4000-8000-0000000000{n:02x}"
        ))
        .unwrap()
    };
    S {
        t: id(1),
        a: id(2),
        e: id(3),
    }
}

fn base(s: S) -> String {
    format!("/internal/v1/push/providers/{}/{}/{}", s.t, s.a, s.e)
}

/// A fresh APNs `.p8` (PKCS#8 P-256 PEM).
fn p8() -> String {
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    let secret = p256::SecretKey::from_slice(&rand::random::<[u8; 32]>()).unwrap();
    secret.to_pkcs8_pem(LineEnding::LF).unwrap().to_string()
}

fn service_account(project: &str) -> String {
    json!({
        "type": "service_account", "project_id": project,
        "client_email": format!("push@{project}.iam.gserviceaccount.com"),
        "private_key": p8(),
    })
    .to_string()
}

fn apns_meta(bundle: &str) -> Value {
    json!({ "teamId": "TEAM123456", "keyId": "KEY1234567", "bundleId": bundle, "apnsHost": "SANDBOX" })
}

fn apns_handle(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn fcm_handle(tag: &str) -> String {
    format!("fcm-registration-token-{tag}-0123456789abcdef")
}

/// What the Developer Console sends: the secret sealed for the scope, its fingerprint
/// and metadata.
fn put_body(h: &Harness, s: S, secret: &str, metadata: Value) -> Value {
    let sealed = seal_push_credential(&h.sealing, s.t, s.a, s.e, secret).unwrap();
    json!({ "sealed": sealed.to_compact(), "metadata": metadata, "fingerprint": push_credential_fingerprint(secret) })
}

async fn put(h: &Harness, s: S, provider: &str, secret: &str, metadata: Value) -> Response {
    let dev = h.peers.developer.clone();
    let body = put_body(h, s, secret, metadata);
    h.signed(
        &dev,
        Method::PUT,
        &format!("{}/{provider}", base(s)),
        Some(body),
        &[],
    )
    .await
}

async fn dev(h: &Harness, method: Method, path: &str) -> Response {
    let dev = h.peers.developer.clone();
    h.signed(&dev, method, path, None, &[]).await
}

fn registration(
    s: S,
    principal: Uuid,
    installation: &str,
    platform: &str,
    handle: Option<&str>,
) -> Value {
    let mut v = json!({
        "subject": { "kind": "platform-principal", "tenantId": s.t, "applicationId": s.a,
                     "environmentId": s.e, "principalId": principal, "installationId": installation },
        "platform": platform,
        "enabled": handle.is_some(),
    });
    if let Some(h) = handle {
        v["handle"] = json!(h);
    }
    v
}

async fn sync(h: &Harness, items: Vec<Value>) -> Response {
    let messaging = h.peers.messaging.clone();
    h.signed(
        &messaging,
        Method::POST,
        SYNC,
        Some(json!({ "registrations": items })),
        &[],
    )
    .await
}

async fn ring(h: &Harness, s: S, principal: Uuid) -> Response {
    let calling = h.peers.calling.clone();
    let body = json!({
        "recipient": { "kind": "platform-principal", "tenantId": s.t, "applicationId": s.a,
                       "environmentId": s.e, "principalId": principal },
        "payload": { "callId": Uuid::new_v4() },
        "expiresAt": (chrono::Utc::now() + chrono::Duration::seconds(45)).to_rfc3339(),
    });
    h.signed(&calling, Method::POST, CALL, Some(body), &[])
        .await
}

fn message_event(s: S, principals: &[Uuid], conversation: Uuid, seq: i64) -> Value {
    let mut b = EnvelopeBuilder::new(
        Producer::Messaging,
        "messaging.message.created",
        NOW,
        route(),
    )
    .unwrap()
    .dedupe_key(format!("messaging.message.created:{}", Uuid::new_v4()))
    .scope(EnvScope {
        tenant_id: s.t,
        application_id: s.a,
        environment_id: s.e,
    })
    .subject(conversation, None);
    for p in principals {
        b = b.recipient(platform_principal_ref(s.e, *p));
    }
    let env = b
        .data("conversationId", conversation.to_string())
        .data("conversationKind", "DIRECT")
        .data("eventSequence", seq)
        .data("securityProfile", "MANAGED")
        .data("sequence", seq)
        .build()
        .unwrap();
    serde_json::to_value(env).unwrap()
}

/// Captures every log line of this test binary (see `push_facade.rs`).
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
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
}

/// The base64 body of a PEM (what must never appear anywhere in clear).
fn pem_body(pem: &str) -> String {
    pem.lines().filter(|l| !l.starts_with("-----")).collect()
}

#[tokio::test]
async fn with_the_flag_off_every_partner_route_and_shape_behaves_as_before() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = scope(1);
    // The vault routes do not exist.
    put(&h, a, "APNS", &p8(), apns_meta("com.example.a"))
        .await
        .expect_error(404, "NOT_FOUND");
    dev(&h, Method::GET, &base(a))
        .await
        .expect_error(404, "NOT_FOUND");
    dev(&h, Method::DELETE, &format!("{}/APNS", base(a)))
        .await
        .expect_error(404, "NOT_FOUND");
    dev(&h, Method::POST, &format!("{}/APNS/verify", base(a)))
        .await
        .expect_error(404, "NOT_FOUND");
    // I13 / I3 refuse the partner shapes exactly as an unknown field.
    sync(
        &h,
        vec![registration(
            a,
            Uuid::new_v4(),
            "inst-0001",
            "APNS",
            Some(&apns_handle(1)),
        )],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    ring(&h, a, Uuid::new_v4())
        .await
        .expect_error(400, "VALIDATION_ERROR");
    // I1 refuses a platform-principal:<env>:<principal> recipient as before.
    let messaging = h.peers.messaging.clone();
    let r = h
        .ingest(
            &messaging,
            vec![message_event(a, &[Uuid::new_v4()], Uuid::new_v4(), 1)],
        )
        .await;
    let out = &r.expect(200)["results"][0];
    assert_eq!(out["outcome"], "REJECTED");
    assert_eq!(out["reason"], "INVALID_FIELD");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_providers")
            .await,
        0
    );
    assert!(h.partner.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_vault_stores_lists_verifies_and_deletes_and_never_gives_the_secret_back() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let logs = Captured::global();
    let a = scope(1);
    let key = p8();
    let r = put(&h, a, "APNS_VOIP", &key, apns_meta("com.example.a.voip")).await;
    let v = r.expect(200).clone();
    assert_eq!(v["provider"], "APNS_VOIP");
    assert_eq!(v["status"], "UNVERIFIED");
    assert_eq!(v["fingerprint"], push_credential_fingerprint(&key));
    assert!(v["updatedAt"].is_string());
    let fcm = service_account("demo-a");
    put(&h, a, "FCM", &fcm, json!({})).await.expect(200);

    let list = dev(&h, Method::GET, &base(a)).await;
    let items = list.expect(200)["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["provider"], "APNS_VOIP");
    // The VoIP topic is derived from the app's bundle id.
    assert_eq!(items[0]["metadata"]["bundleId"], "com.example.a");
    assert_eq!(items[1]["provider"], "FCM");
    assert_eq!(items[1]["metadata"]["projectId"], "demo-a");

    // Verify (a live check through the transport), then a refused one.
    let r = dev(&h, Method::POST, &format!("{}/APNS_VOIP/verify", base(a))).await;
    assert_eq!(r.expect(200), &json!({ "status": "VERIFIED" }));
    *h.partner.verify_failure.lock().unwrap() = Some("PROVIDER_TOKEN_REFUSED");
    let r = dev(&h, Method::POST, &format!("{}/FCM/verify", base(a))).await;
    assert_eq!(
        r.expect(200),
        &json!({ "status": "FAILED", "lastError": "PROVIDER_TOKEN_REFUSED" })
    );
    let list = dev(&h, Method::GET, &base(a)).await;
    let items = list.expect(200)["items"].as_array().unwrap().clone();
    assert_eq!(items[0]["status"], "VERIFIED");
    assert!(items[0]["lastVerifiedAt"].is_string());
    assert_eq!(items[1]["status"], "FAILED");
    assert_eq!(items[1]["lastError"], "PROVIDER_TOKEN_REFUSED");

    // The secret is never readable back: not in any response, not in the database (only a
    // data-key seal), not in the logs.
    let everything = format!("{v}{list:?}");
    for secret in [
        pem_body(&key),
        pem_body(
            serde_json::from_str::<Value>(&fcm).unwrap()["private_key"]
                .as_str()
                .unwrap(),
        ),
    ] {
        assert!(!everything.contains(&secret));
        let rows = h
            .text("SELECT string_agg(row_to_json(p)::text, '') FROM notif_push_providers p")
            .await
            .unwrap();
        assert!(!rows.contains(&secret[..40]));
        assert!(!logs.text().contains(&secret[..40]));
    }
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_providers WHERE secret_sealed LIKE 'v2.test-1.%'").await,
        2
    );
    // The database refuses anything but a seal.
    assert!(h
        .exec(&format!(
            "UPDATE notif_push_providers SET secret_sealed = '{}' WHERE provider = 'FCM'",
            "-----BEGIN PRIVATE KEY-----"
        ))
        .await
        .is_err());

    // Replacing a credential resets its status.
    let key2 = p8();
    let r = put(&h, a, "APNS_VOIP", &key2, apns_meta("com.example.a")).await;
    assert_eq!(r.expect(200)["status"], "UNVERIFIED");
    assert_eq!(r.json["fingerprint"], push_credential_fingerprint(&key2));

    // Delete is idempotent; verify of a missing credential is 404.
    dev(&h, Method::DELETE, &format!("{}/APNS_VOIP", base(a)))
        .await
        .expect(204);
    dev(&h, Method::DELETE, &format!("{}/APNS_VOIP", base(a)))
        .await
        .expect(204);
    dev(&h, Method::POST, &format!("{}/APNS_VOIP/verify", base(a)))
        .await
        .expect_error(404, "NOT_FOUND");
    let list = dev(&h, Method::GET, &base(a)).await;
    assert_eq!(list.expect(200)["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        h.state.metrics.get(
            "notif_partner_push_providers_total",
            &format!(
                "environment=\"{}\",provider=\"APNS_VOIP\",action=\"DELETE\"",
                a.e
            )
        ),
        1
    );
}

#[tokio::test]
async fn the_vault_refuses_other_callers_scopes_seals_and_bad_input() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let (a, b) = (scope(1), scope(2));
    let key = p8();
    // Only the Developer Console holds notifications-push-provider.manage.
    let messaging = h.peers.messaging.clone();
    h.signed(
        &messaging,
        Method::PUT,
        &format!("{}/APNS", base(a)),
        Some(put_body(&h, a, &key, apns_meta("com.example.a"))),
        &[],
    )
    .await
    .expect_error(403, "FORBIDDEN");
    // A credential sealed for scope B never opens under scope A.
    let dev_peer = h.peers.developer.clone();
    h.signed(
        &dev_peer,
        Method::PUT,
        &format!("{}/APNS", base(a)),
        Some(put_body(&h, b, &key, apns_meta("com.example.a"))),
        &[],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    // A fingerprint that is not the secret's.
    let mut body = put_body(&h, a, &key, apns_meta("com.example.a"));
    body["fingerprint"] = json!(push_credential_fingerprint("something else"));
    h.signed(
        &dev_peer,
        Method::PUT,
        &format!("{}/APNS", base(a)),
        Some(body),
        &[],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    // Plaintext instead of a seal, bad metadata, a key that is not a key, unknown slot.
    let mut body = put_body(&h, a, &key, apns_meta("com.example.a"));
    body["sealed"] = json!(key);
    h.signed(
        &dev_peer,
        Method::PUT,
        &format!("{}/APNS", base(a)),
        Some(body),
        &[],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    put(
        &h,
        a,
        "APNS",
        &key,
        json!({ "teamId": "TEAM123456", "keyId": "KEY1234567", "bundleId": "com.example.a" }),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    put(&h, a, "APNS", &key, json!({ "projectId": "demo" }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    put(&h, a, "APNS", "not a key", apns_meta("com.example.a"))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    put(
        &h,
        a,
        "FCM",
        &service_account("demo"),
        json!({ "projectId": "other" }),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    put(&h, a, "WEB_PUSH", &key, json!({}))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    dev(
        &h,
        Method::GET,
        "/internal/v1/push/providers/not-a-uuid/x/y",
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_providers")
            .await,
        0
    );
    // An environment's credential belongs to one (tenant, application) only.
    put(&h, a, "APNS", &key, apns_meta("com.example.a"))
        .await
        .expect(200);
    let hijack = S {
        t: b.t,
        a: b.a,
        e: a.e,
    };
    put(&h, hijack, "APNS", &p8(), apns_meta("com.example.b"))
        .await
        .expect_error(409, "VERSION_CONFLICT");
    // ... and another tenant cannot list or delete it.
    let list = dev(&h, Method::GET, &base(hijack)).await;
    assert_eq!(list.expect(200)["items"], json!([]));
    dev(&h, Method::DELETE, &format!("{}/APNS", base(hijack)))
        .await
        .expect(204);
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_providers")
            .await,
        1
    );
}

#[tokio::test]
async fn messaging_registers_and_removes_platform_principal_installations() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let a = scope(1);
    let p = Uuid::new_v4();
    let voip = apns_handle(0xa1);
    let fcm = fcm_handle("android-1");
    // Nested and flat subjects, in one batch with a first-party item (unchanged path).
    let mut flat = json!({
        "kind": "platform-principal", "tenantId": a.t, "applicationId": a.a, "environmentId": a.e,
        "principalId": p, "installationId": "inst-android-1", "platform": "FCM", "handle": fcm, "enabled": true,
    });
    let first_party = json!({
        "tenantId": "7a000000-0000-4000-8000-000000000001", "applicationId": "7a000000-0000-4000-8000-000000000002",
        "environmentId": "7a000000-0000-4000-8000-000000000003", "principalId": "7a000000-0000-4000-8000-0000000000aa",
        "deviceId": Uuid::new_v4(), "accountRef": alice(), "platform": "LOCAL_TEST", "handle": "local-handle-1", "enabled": true,
    });
    let r = sync(
        &h,
        vec![
            registration(a, p, "inst-ios-0001", "APNS_VOIP", Some(&voip)),
            flat.clone(),
            first_party,
        ],
    )
    .await;
    let out = r.expect(200)["results"].as_array().unwrap().clone();
    assert_eq!(out[0]["kind"], "platform-principal");
    assert_eq!(out[0]["installationId"], "inst-ios-0001");
    assert_eq!(out[0]["platform"], "APNS_VOIP");
    assert_eq!(out[0]["outcome"], "CREATED");
    assert!(out[0]["pushTargetRef"].is_string());
    assert_eq!(out[1]["outcome"], "CREATED");
    assert_eq!(out[2]["outcome"], "CREATED");
    assert!(out[2]["deviceId"].is_string());
    // Idempotent replay.
    let again = sync(
        &h,
        vec![registration(
            a,
            p,
            "inst-ios-0001",
            "APNS_VOIP",
            Some(&voip),
        )],
    )
    .await;
    assert_eq!(again.expect(200)["results"][0]["outcome"], "UNCHANGED");
    assert_eq!(
        again.json["results"][0]["pushTargetRef"],
        out[0]["pushTargetRef"]
    );
    // Stored keyed by (scope, principal, installation, platform), handle sealed, no account.
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_push_endpoints WHERE subject_kind = 'platform-principal'
               AND tenant_id = '{}' AND application_id = '{}' AND environment_id = '{}' AND principal_id = '{p}'
               AND account_ref IS NULL AND handle_sealed LIKE 'v2.%' AND installation_id IN ('inst-ios-0001', 'inst-android-1')",
            a.t, a.a, a.e
        ))
        .await,
        2
    );
    let dump = h
        .text("SELECT string_agg(row_to_json(e)::text, '') FROM notif_push_endpoints e")
        .await
        .unwrap();
    assert!(!dump.contains(&voip) && !dump.contains(&fcm));
    // Invalid items: platform, installation id, handle, kind.
    let bad = sync(
        &h,
        vec![
            registration(a, p, "inst-ios-0001", "WEB_PUSH", Some("x")),
            registration(a, p, "short", "APNS", Some(&apns_handle(2))),
            registration(a, p, "inst-ios-0002", "APNS", Some("not-hex")),
            {
                let mut v = registration(a, p, "inst-ios-0003", "APNS", Some(&apns_handle(3)));
                v["subject"]["kind"] = json!("cfmeet-user");
                v
            },
        ],
    )
    .await;
    let reasons: Vec<Value> = bad.expect(200)["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| json!([r["outcome"], r["reason"]]))
        .collect();
    assert_eq!(
        reasons,
        vec![
            json!(["INVALID", "PLATFORM"]),
            json!(["INVALID", "INSTALLATION"]),
            json!(["INVALID", "HANDLE"]),
            json!(["INVALID", "SUBJECT"]),
        ]
    );
    // Removal: enabled false with the platform (flat), then without one (nested).
    flat["enabled"] = json!(false);
    flat.as_object_mut().unwrap().remove("handle");
    let removed = sync(&h, vec![flat.clone()]).await;
    assert_eq!(removed.expect(200)["results"][0]["outcome"], "REMOVED");
    assert_eq!(
        sync(&h, vec![flat]).await.expect(200)["results"][0]["outcome"],
        "ABSENT"
    );
    let mut all = registration(a, p, "inst-ios-0001", "APNS_VOIP", None);
    all.as_object_mut().unwrap().remove("platform");
    assert_eq!(
        sync(&h, vec![all]).await.expect(200)["results"][0]["outcome"],
        "REMOVED"
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE subject_kind IS NOT NULL")
            .await,
        0
    );
    // The first-party copy is untouched.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE account_ref IS NOT NULL")
            .await,
        1
    );
}

/// Sets up scopes A and B with their own credentials and one principal each.
async fn two_partners(h: &Harness) -> (S, S, Uuid, Uuid, String, String) {
    let (a, b) = (scope(1), scope(2));
    let (ka, kb) = (p8(), p8());
    put(h, a, "APNS_VOIP", &ka, apns_meta("com.example.a"))
        .await
        .expect(200);
    put(h, a, "APNS", &ka, apns_meta("com.example.a"))
        .await
        .expect(200);
    put(h, a, "FCM", &service_account("demo-a"), json!({}))
        .await
        .expect(200);
    put(h, b, "APNS_VOIP", &kb, apns_meta("com.example.b"))
        .await
        .expect(200);
    let (pa, pb) = (Uuid::new_v4(), Uuid::new_v4());
    let r = sync(
        h,
        vec![
            registration(a, pa, "inst-a-ios-1", "APNS_VOIP", Some(&apns_handle(0xa1))),
            registration(a, pa, "inst-a-ios-1", "APNS", Some(&apns_handle(0xa2))),
            registration(a, pa, "inst-a-android", "FCM", Some(&fcm_handle("a"))),
            registration(b, pb, "inst-b-ios-1", "APNS_VOIP", Some(&apns_handle(0xb1))),
        ],
    )
    .await;
    assert!(r.expect(200)["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["outcome"] == "CREATED"));
    (
        a,
        b,
        pa,
        pb,
        push_credential_fingerprint(&ka),
        push_credential_fingerprint(&kb),
    )
}

#[tokio::test]
async fn the_call_fast_lane_rings_voip_and_fcm_with_the_recipients_own_credentials() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let (a, b, pa, pb, fa, fb) = two_partners(&h).await;
    // CALL_INVITATIONS routing is OFF (the first-party cutover): partner calls still ring.
    let r = ring(&h, a, pa).await;
    let v = r.expect(200).clone();
    assert_eq!(v["recipientStatus"], "SENT");
    let outcomes: Vec<&Value> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["outcome"])
        .collect();
    assert_eq!(outcomes, vec!["SENT", "SENT"]);
    {
        let sent = h.partner.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        let voip = sent.iter().find(|s| s.provider == "APNS_VOIP").unwrap();
        assert_eq!(voip.scope.environment_id, a.e);
        assert_eq!(voip.fingerprint, fa);
        assert_eq!(voip.platform, "APNS");
        assert_eq!(voip.topic.as_deref(), Some("com.example.a.voip"));
        assert!(voip.options.voip);
        assert_eq!(voip.handle, apns_handle(0xa1));
        assert_eq!(voip.payload, json!({ "v": 1, "type": "call" }));
        assert_eq!(voip.options.collapse_id.as_deref(), Some("cfmeet-call"));
        assert!(voip.options.ttl_seconds.unwrap() <= 45);
        let fcm = sent.iter().find(|s| s.provider == "FCM").unwrap();
        assert_eq!(fcm.scope.environment_id, a.e);
        assert_eq!(fcm.platform, "FCM");
        assert_eq!(fcm.topic, None);
        assert!(!fcm.options.voip);
        // The APNs alert installation never rings a call.
        assert!(!sent.iter().any(|s| s.handle == apns_handle(0xa2)));
    }
    // CF Meet's own adapter is never used for partner recipients.
    assert!(h.push.sent.lock().unwrap().is_empty());
    h.partner.sent.lock().unwrap().clear();

    // B's principal rings with B's credential only.
    let r = ring(&h, b, pb).await;
    assert_eq!(r.expect(200)["recipientStatus"], "SENT");
    {
        let sent = h.partner.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].scope.environment_id, b.e);
        assert_eq!(sent[0].fingerprint, fb);
        assert_eq!(sent[0].topic.as_deref(), Some("com.example.b.voip"));
    }
    h.partner.sent.lock().unwrap().clear();

    // A forged recipient (B's environment under A's tenant / application) reaches nothing.
    let forged = S {
        t: a.t,
        a: a.a,
        e: b.e,
    };
    let r = ring(&h, forged, pb).await;
    assert_eq!(r.expect(200)["recipientStatus"], "NO_REGISTRATION");
    // A's principal addressed in B's scope reaches nothing either.
    assert_eq!(
        ring(&h, b, pa).await.expect(200)["recipientStatus"],
        "NO_REGISTRATION"
    );

    // Scope C: registered, but no credential → nothing is sent, clear status.
    let c = scope(3);
    let pc = Uuid::new_v4();
    sync(
        &h,
        vec![registration(
            c,
            pc,
            "inst-c-ios-1",
            "APNS_VOIP",
            Some(&apns_handle(0xc1)),
        )],
    )
    .await
    .expect(200);
    let r = ring(&h, c, pc).await;
    let v = r.expect(200).clone();
    assert_eq!(v["recipientStatus"], "NO_CREDENTIAL");
    assert_eq!(v["results"][0]["outcome"], "NOT_CONFIGURED");
    assert!(h.partner.sent.lock().unwrap().is_empty());

    // A refused handle (APNs 410 / FCM UNREGISTERED) disables the installation.
    h.partner
        .invalid_handles
        .lock()
        .unwrap()
        .push(fcm_handle("a"));
    let r = ring(&h, a, pa).await;
    let outcomes: Vec<Value> = r.expect(200)["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["outcome"].clone())
        .collect();
    assert!(outcomes.contains(&json!("INVALID_HANDLE")));
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE platform = 'FCM' AND NOT enabled AND disabled_reason = 'INVALID_HANDLE'").await,
        1
    );
    let r = ring(&h, a, pa).await;
    let outcomes: Vec<Value> = r.expect(200)["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["outcome"].clone())
        .collect();
    assert!(outcomes.contains(&json!("DISABLED")));

    // Expired invitations never ring.
    let calling = h.peers.calling.clone();
    let late = h
        .signed(
            &calling,
            Method::POST,
            CALL,
            Some(json!({
                "recipient": { "kind": "platform-principal", "tenantId": a.t, "applicationId": a.a, "environmentId": a.e, "principalId": pa },
                "payload": {}, "expiresAt": "2020-01-01T00:00:00Z",
            })),
            &[],
        )
        .await;
    assert_eq!(late.expect(200)["recipientStatus"], "EXPIRED");

    // N-P5: per-scope counters.
    let m = &h.state.metrics;
    assert_eq!(
        m.get(
            "notif_partner_push_sent_total",
            &format!(
                "environment=\"{}\",lane=\"call\",platform=\"APNS_VOIP\"",
                a.e
            )
        ),
        3
    );
    assert_eq!(
        m.get(
            "notif_partner_push_sent_total",
            &format!(
                "environment=\"{}\",lane=\"call\",platform=\"APNS_VOIP\"",
                b.e
            )
        ),
        1
    );
    assert_eq!(
        m.get(
            "notif_partner_push_failed_total",
            &format!(
                "environment=\"{}\",lane=\"call\",platform=\"APNS_VOIP\",class=\"NOT_CONFIGURED\"",
                c.e
            )
        ),
        1
    );
    // The existing first-party shape is untouched.
    let r = h
        .signed(
            &calling,
            Method::POST,
            CALL,
            Some(json!({ "pushTargetRefs": [Uuid::new_v4()], "payload": {}, "expiresAt": "2030-01-01T00:00:00Z" })),
            &[],
        )
        .await;
    assert_eq!(r.expect(200)["results"][0]["outcome"], "NOT_CONFIGURED");
}

#[tokio::test]
async fn partner_message_events_are_metadata_only_pushes_with_the_partner_credential() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let (a, _b, pa, _pb, _fa, _fb) = two_partners(&h).await;
    let c = scope(3);
    let pc = Uuid::new_v4();
    sync(
        &h,
        vec![registration(
            c,
            pc,
            "inst-c-android",
            "FCM",
            Some(&fcm_handle("c")),
        )],
    )
    .await
    .expect(200);
    let conversation = Uuid::new_v4();
    let messaging = h.peers.messaging.clone();
    // MESSAGE_WAKEUPS is OFF (first-party cutover): partner events are not HELD.
    let r = h
        .ingest(&messaging, vec![message_event(a, &[pa], conversation, 1)])
        .await;
    assert_eq!(r.expect(200)["results"][0]["outcome"], "ACCEPTED");
    h.process_all().await;
    let r = h
        .ingest(
            &messaging,
            vec![
                message_event(a, &[pa], conversation, 2),
                message_event(c, &[pc], conversation, 3),
            ],
        )
        .await;
    let out = r.expect(200)["results"].as_array().unwrap().clone();
    assert!(out.iter().all(|o| o["outcome"] == "ACCEPTED"), "{out:?}");
    h.process_all().await;
    // One notification per event and recipient; the older message of the conversation is
    // collapsed before it was sent.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_deliveries WHERE status = 'CANCELLED' AND last_error_class = 'COLLAPSED'").await,
        2
    );
    // The first-party dispatcher never claims partner deliveries.
    let fp = h.push_worker().tick(chrono::Utc::now()).await.unwrap();
    assert_eq!(fp.claimed, 0);
    let tick = notifications::partner::PartnerPushWorker::new(
        h.state.db.clone(),
        h.state.keys.clone(),
        h.state.partner.clone().unwrap(),
        h.state.metrics.clone(),
    )
    .tick(chrono::Utc::now())
    .await
    .unwrap();
    assert_eq!((tick.claimed, tick.sent, tick.failed), (3, 2, 1));
    let sent = h.partner.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    for s in &sent {
        assert_eq!(s.scope.environment_id, a.e);
        assert!(!s.options.voip);
        assert_eq!(
            s.options.collapse_id,
            Some(conversation.simple().to_string())
        );
        assert_eq!(
            s.payload,
            json!({ "v": 1, "type": "message", "conversationId": conversation.to_string(), "eventSequence": 2, "sequence": 2 })
        );
    }
    let apns = sent.iter().find(|s| s.platform == "APNS").unwrap();
    assert_eq!(apns.provider, "APNS");
    assert_eq!(apns.topic.as_deref(), Some("com.example.a"));
    assert_eq!(apns.handle, apns_handle(0xa2));
    assert!(sent
        .iter()
        .any(|s| s.provider == "FCM" && s.handle == fcm_handle("a")));
    // VoIP installations never get message pushes; CF Meet's adapter is never used.
    assert!(!sent.iter().any(|s| s.handle == apns_handle(0xa1)));
    assert!(h.push.sent.lock().unwrap().is_empty());
    // Scope C has no credential: failed with a clear class, nothing sent.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_deliveries WHERE status = 'FAILED' AND last_error_class = 'NO_CREDENTIAL'").await,
        1
    );
    assert_eq!(
        h.state.metrics.get(
            "notif_partner_push_sent_total",
            &format!("environment=\"{}\",lane=\"message\",platform=\"APNS\"", a.e)
        ),
        1
    );
    assert_eq!(
        h.state.metrics.get(
            "notif_partner_push_failed_total",
            &format!(
                "environment=\"{}\",lane=\"message\",platform=\"FCM\",class=\"NO_CREDENTIAL\"",
                c.e
            )
        ),
        1
    );
    // Partner notifications keep ids only.
    let params = h
        .text("SELECT string_agg(params::text, '') FROM notif_notifications WHERE recipient_ref LIKE 'platform-principal:%'")
        .await
        .unwrap();
    assert!(!params.contains("MANAGED") && !params.contains("DIRECT"));
}

#[tokio::test]
async fn a_deleted_or_moved_credential_never_sends_and_refused_handles_are_disabled() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let (a, b, pa, _pb, _fa, _fb) = two_partners(&h).await;
    let worker = notifications::partner::PartnerPushWorker::new(
        h.state.db.clone(),
        h.state.keys.clone(),
        h.state.partner.clone().unwrap(),
        h.state.metrics.clone(),
    );
    let messaging = h.peers.messaging.clone();
    // Revocation stops sends at once.
    dev(&h, Method::DELETE, &format!("{}/FCM", base(a)))
        .await
        .expect(204);
    h.ingest(&messaging, vec![message_event(a, &[pa], Uuid::new_v4(), 1)])
        .await
        .expect(200);
    h.process_all().await;
    let t = worker.tick(chrono::Utc::now()).await.unwrap();
    assert_eq!((t.sent, t.failed), (1, 1));
    assert!(h
        .partner
        .sent
        .lock()
        .unwrap()
        .iter()
        .all(|s| s.platform == "APNS"));
    h.partner.sent.lock().unwrap().clear();

    // A's sealed credential copied onto B's row never opens there (bound to its scope).
    h.exec(&format!(
        "UPDATE notif_push_providers SET secret_sealed = (SELECT secret_sealed FROM notif_push_providers
            WHERE environment_id = '{}' AND provider = 'APNS_VOIP') WHERE environment_id = '{}' AND provider = 'APNS_VOIP'",
        a.e, b.e
    ))
    .await
    .unwrap();
    let pb = Uuid::parse_str(
        &h.text(&format!(
            "SELECT principal_id::text FROM notif_push_endpoints WHERE environment_id = '{}'",
            b.e
        ))
        .await
        .unwrap(),
    )
    .unwrap();
    let r = ring(&h, b, pb).await;
    assert_eq!(r.expect(200)["results"][0]["outcome"], "PERMANENT_FAILURE");
    assert!(h.partner.sent.lock().unwrap().is_empty());

    // APNs 410 on a message push: the installation is disabled.
    h.partner
        .invalid_handles
        .lock()
        .unwrap()
        .push(apns_handle(0xa2));
    h.ingest(&messaging, vec![message_event(a, &[pa], Uuid::new_v4(), 2)])
        .await
        .expect(200);
    h.process_all().await;
    let t = worker.tick(chrono::Utc::now()).await.unwrap();
    // APNs refused the handle; FCM still has no credential.
    assert_eq!((t.sent, t.failed), (0, 2));
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE platform = 'APNS' AND NOT enabled AND disabled_reason = 'INVALID_HANDLE'").await,
        1
    );
    assert_eq!(
        h.state.metrics.get(
            "notif_partner_push_failed_total",
            &format!(
                "environment=\"{}\",lane=\"message\",platform=\"APNS\",class=\"INVALID_HANDLE\"",
                a.e
            )
        ),
        1
    );
}

/// The Developer Console's multi-part form (`services/developer-platform/src/push_providers.rs`
/// `seal_secret`): base64url of the secret in ≤ 512-character `token` parts, each sealed
/// with `seal` for (`cfmeet.developer-platform`, `<t>/<a>/<e>/<provider>`,
/// `push-provider.secret.<i>-of-<n>`).
fn console_parts(h: &Harness, s: S, provider: &str, secret: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
    let encoded = B64.encode(secret.as_bytes());
    let count = encoded.len().div_ceil(512);
    let size = encoded.len().div_ceil(count);
    let event_id = format!("{}/{}/{}/{provider}", s.t, s.a, s.e);
    let parts: Vec<_> = encoded
        .as_bytes()
        .chunks(size)
        .enumerate()
        .map(|(i, chunk)| {
            cf_notification_client::seal::seal(
                &h.sealing,
                "cfmeet.developer-platform",
                &event_id,
                &format!("push-provider.secret.{i}-of-{count}"),
                &cf_notification_client::SealedPlaintext::default()
                    .with_token(std::str::from_utf8(chunk).unwrap()),
            )
            .unwrap()
        })
        .collect();
    json!({ "v": 1, "parts": parts }).to_string()
}

#[tokio::test]
async fn the_vault_opens_the_consoles_multi_part_seals_for_their_own_slot_only() {
    let Some(h) = Harness::with_partner_push().await else {
        return;
    };
    let (a, b) = (scope(1), scope(2));
    let dev_peer = h.peers.developer.clone();
    // A service account (real ones are about 2.3 KB) spans several parts.
    let mut account: Value = serde_json::from_str(&service_account("demo-multi")).unwrap();
    account["client_x509_cert_url"] = json!(format!(
        "https://www.googleapis.com/robot/v1/metadata/x509/{}",
        "p".repeat(1500)
    ));
    let account = account.to_string();
    let sealed = console_parts(&h, a, "FCM", &account);
    assert!(
        serde_json::from_str::<Value>(&sealed).unwrap()["parts"]
            .as_array()
            .unwrap()
            .len()
            > 1
    );
    let body = json!({ "sealed": sealed, "metadata": { "projectId": "demo-multi" },
                       "fingerprint": push_credential_fingerprint(&account) });
    let r = h
        .signed(
            &dev_peer,
            Method::PUT,
            &format!("{}/FCM", base(a)),
            Some(body),
            &[],
        )
        .await;
    assert_eq!(r.expect(200)["status"], "UNVERIFIED");
    // A p8 sealed for the APNS_VOIP slot opens there, not in the APNS slot or another scope.
    let key = p8();
    let sealed = console_parts(&h, a, "APNS_VOIP", &key);
    let body = |sealed: &str| {
        json!({ "sealed": sealed, "metadata": apns_meta("com.example.a"),
                "fingerprint": push_credential_fingerprint(&key) })
    };
    for (s, slot) in [(a, "APNS"), (b, "APNS_VOIP")] {
        h.signed(
            &dev_peer,
            Method::PUT,
            &format!("{}/{slot}", base(s)),
            Some(body(&sealed)),
            &[],
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    }
    h.signed(
        &dev_peer,
        Method::PUT,
        &format!("{}/APNS_VOIP", base(a)),
        Some(body(&sealed)),
        &[],
    )
    .await
    .expect(200);
    // Parts out of order do not open.
    let mut v: Value = serde_json::from_str(&console_parts(&h, a, "FCM", &account)).unwrap();
    v["parts"].as_array_mut().unwrap().reverse();
    let r = h
        .signed(
            &dev_peer,
            Method::PUT,
            &format!("{}/FCM", base(a)),
            Some(json!({ "sealed": v.to_string(), "metadata": {}, "fingerprint": push_credential_fingerprint(&account) })),
            &[],
        )
        .await;
    r.expect_error(400, "VALIDATION_ERROR");
    // The stored credentials send.
    let p = Uuid::new_v4();
    sync(
        &h,
        vec![registration(
            a,
            p,
            "inst-a-multi-1",
            "APNS_VOIP",
            Some(&apns_handle(0x5a)),
        )],
    )
    .await
    .expect(200);
    assert_eq!(ring(&h, a, p).await.expect(200)["recipientStatus"], "SENT");
}
