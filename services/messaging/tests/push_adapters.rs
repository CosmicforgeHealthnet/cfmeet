//! The live push adapters (APNs, FCM HTTP v1, Web Push) driven by the real dispatch worker
//! against DETERMINISTIC LOCAL FAKES of each service. The fakes enforce what the real
//! services require — an ES256 APNs provider token signed by the configured key, an RS256
//! service-account assertion exchanged for an OAuth token before FCM accepts a send, a
//! VAPID token for the push service's origin and an RFC 8291 body the subscriber can
//! decrypt — and answer with scripted provider errors. This is NOT live certification
//! against Apple, Google or a browser vendor (that needs their accounts and credentials).

mod common;

use std::{
    collections::VecDeque,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use aes_gcm::{aead::Aead, Aes128Gcm, KeyInit};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode as S},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use common::{flows::*, realtime::*, *};
use hmac::{Hmac, Mac};
use messaging::realtime::push_adapters::{
    Apns, ApnsSettings, Fcm, FcmSettings, PushAdapters, ServiceAccount, WebPush, WebPushSettings,
};
use p256::{
    ecdsa::{signature::Verifier, Signature, VerifyingKey},
    elliptic_curve::sec1::ToEncodedPoint,
    pkcs8::{EncodePrivateKey, LineEnding},
    PublicKey, SecretKey,
};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

#[derive(Default)]
struct Fake {
    /// Scripted answers (status, body) consumed before the default success.
    script: Mutex<VecDeque<(u16, Value)>>,
    /// Every accepted notification's (handle, decoded payload).
    received: Mutex<Vec<(String, Value)>>,
    /// Requests refused for bad authentication.
    refused: Mutex<usize>,
    tokens_issued: Mutex<Vec<String>>,
}

impl Fake {
    fn next(&self) -> Option<(u16, Value)> {
        self.script.lock().unwrap().pop_front()
    }
    fn script(&self, answers: &[(u16, Value)]) {
        self.script.lock().unwrap().extend(answers.iter().cloned());
    }
    fn received(&self) -> Vec<(String, Value)> {
        self.received.lock().unwrap().clone()
    }
}

fn jwt_parts(t: &str) -> (Value, Value, String, Vec<u8>) {
    let mut it = t.split('.');
    let (h, c, s) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    (
        serde_json::from_slice(&B64URL.decode(h).unwrap()).unwrap(),
        serde_json::from_slice(&B64URL.decode(c).unwrap()).unwrap(),
        format!("{h}.{c}"),
        B64URL.decode(s).unwrap(),
    )
}

fn es256_ok(key: &VerifyingKey, t: &str) -> Option<(Value, Value)> {
    let (h, c, input, sig) = jwt_parts(t);
    let sig = Signature::from_slice(&sig).ok()?;
    key.verify(input.as_bytes(), &sig).ok()?;
    (h["alg"] == "ES256").then_some((h, c))
}

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

// --- APNs fake --------------------------------------------------------------------------

#[derive(Clone)]
struct ApnsFake {
    fake: Arc<Fake>,
    key: VerifyingKey,
}

async fn apns_send(
    State(s): State<ApnsFake>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (S, Json<Value>) {
    let bearer = headers["authorization"].to_str().unwrap();
    let Some((h, c)) = bearer
        .strip_prefix("bearer ")
        .and_then(|t| es256_ok(&s.key, t))
    else {
        *s.fake.refused.lock().unwrap() += 1;
        return (
            S::FORBIDDEN,
            Json(json!({ "reason": "InvalidProviderToken" })),
        );
    };
    assert_eq!(
        (h["kid"].as_str(), c["iss"].as_str()),
        (Some("KEYID12345"), Some("TEAMID1234"))
    );
    assert_eq!(headers["apns-topic"], "com.cfmeet.app");
    assert_eq!(headers["apns-push-type"], "alert");
    if let Some((status, b)) = s.fake.next() {
        return (S::from_u16(status).unwrap(), Json(b));
    }
    s.fake.received.lock().unwrap().push((token, body));
    (S::OK, Json(json!({})))
}

// --- FCM fake ---------------------------------------------------------------------------

#[derive(Clone)]
struct FcmFake {
    fake: Arc<Fake>,
    /// PKCS#1 RSAPublicKey DER of the service account.
    public_der: Arc<Vec<u8>>,
    token_uri: Arc<Mutex<String>>,
}

async fn fcm_token(State(s): State<FcmFake>, body: String) -> (S, Json<Value>) {
    let mut grant = None;
    let mut assertion = None;
    for kv in body.split('&') {
        match kv.split_once('=') {
            Some(("grant_type", v)) => grant = Some(v.to_string()),
            Some(("assertion", v)) => assertion = Some(v.to_string()),
            _ => {}
        }
    }
    assert_eq!(
        grant.as_deref(),
        Some("urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer")
    );
    let (h, c, input, sig) = jwt_parts(&assertion.unwrap());
    let key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        s.public_der.as_slice(),
    );
    if h["alg"] != "RS256" || key.verify(input.as_bytes(), &sig).is_err() {
        *s.fake.refused.lock().unwrap() += 1;
        return (S::BAD_REQUEST, Json(json!({ "error": "invalid_grant" })));
    }
    assert_eq!(c["iss"], "push@cfmeet-test.iam.gserviceaccount.com");
    assert_eq!(
        c["scope"],
        "https://www.googleapis.com/auth/firebase.messaging"
    );
    assert_eq!(
        c["aud"].as_str(),
        Some(s.token_uri.lock().unwrap().as_str())
    );
    let t = format!("ya29.fake-{}", Uuid::new_v4().simple());
    s.fake.tokens_issued.lock().unwrap().push(t.clone());
    (
        S::OK,
        Json(json!({ "access_token": t, "expires_in": 3600, "token_type": "Bearer" })),
    )
}

async fn fcm_send(
    State(s): State<FcmFake>,
    Path((project, action)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (S, Json<Value>) {
    assert_eq!(
        (project.as_str(), action.as_str()),
        ("cfmeet-test", "messages:send")
    );
    let bearer = headers["authorization"]
        .to_str()
        .unwrap()
        .trim_start_matches("Bearer ")
        .to_string();
    if !s.fake.tokens_issued.lock().unwrap().contains(&bearer) {
        *s.fake.refused.lock().unwrap() += 1;
        return (
            S::UNAUTHORIZED,
            Json(json!({ "error": { "status": "UNAUTHENTICATED" } })),
        );
    }
    if let Some((status, b)) = s.fake.next() {
        return (S::from_u16(status).unwrap(), Json(b));
    }
    let m = &body["message"];
    assert!(m.get("notification").is_none(), "data-only: {body}");
    s.fake
        .received
        .lock()
        .unwrap()
        .push((m["token"].as_str().unwrap().to_string(), m["data"].clone()));
    (
        S::OK,
        Json(json!({ "name": "projects/cfmeet-test/messages/1" })),
    )
}

// --- Web Push fake (the push service + the browser's decryption) ------------------------

#[derive(Clone)]
struct WebFake {
    fake: Arc<Fake>,
    origin: Arc<Mutex<String>>,
    ua_secret: Arc<SecretKey>,
    auth: [u8; 16],
}

fn hmac256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).unwrap();
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// The subscriber's side of RFC 8291 (written independently of the adapter).
fn decrypt(body: &[u8], ua_secret: &SecretKey, auth: &[u8; 16]) -> Vec<u8> {
    let salt = &body[..16];
    let rs = u32::from_be_bytes(body[16..20].try_into().unwrap());
    assert_eq!(rs, 4096);
    let idlen = body[20] as usize;
    assert_eq!(idlen, 65);
    let as_public = PublicKey::from_sec1_bytes(&body[21..21 + idlen]).unwrap();
    let ct = &body[21 + idlen..];
    let ua_public = ua_secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), as_public.as_affine());
    let prk_key = hmac256(auth, &[&shared.raw_secret_bytes()[..]]);
    let ikm = hmac256(
        &prk_key,
        &[
            b"WebPush: info\0",
            ua_public.as_bytes(),
            &body[21..21 + idlen],
            &[1],
        ],
    );
    let prk = hmac256(salt, &[&ikm]);
    let cek = hmac256(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]]);
    let nonce = hmac256(&prk, &[b"Content-Encoding: nonce\0", &[1]]);
    let mut pt = Aes128Gcm::new_from_slice(&cek[..16])
        .unwrap()
        .decrypt(&<[u8; 12]>::try_from(&nonce[..12]).unwrap().into(), ct)
        .expect("the subscriber decrypts");
    assert_eq!(pt.pop(), Some(2), "single last record");
    pt
}

async fn web_push(
    State(s): State<WebFake>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> S {
    let auth = headers["authorization"].to_str().unwrap();
    let rest = auth.strip_prefix("vapid ").unwrap();
    let (t, k) = rest.split_once(", ").unwrap();
    let (t, k) = (t.strip_prefix("t=").unwrap(), k.strip_prefix("k=").unwrap());
    let key = VerifyingKey::from_sec1_bytes(&B64URL.decode(k).unwrap()).unwrap();
    let Some((_, c)) = es256_ok(&key, t) else {
        *s.fake.refused.lock().unwrap() += 1;
        return S::UNAUTHORIZED;
    };
    assert_eq!(c["aud"].as_str(), Some(s.origin.lock().unwrap().as_str()));
    let exp = c["exp"].as_i64().unwrap() - chrono::Utc::now().timestamp();
    assert!((0..=86_400).contains(&exp), "RFC 8292: at most 24 hours");
    assert_eq!(c["sub"], "mailto:push-ops@cfmeet.test");
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert!(headers.contains_key("ttl"));
    if let Some((status, _)) = s.fake.next() {
        return S::from_u16(status).unwrap();
    }
    let pt = decrypt(&body, &s.ua_secret, &s.auth);
    s.fake
        .received
        .lock()
        .unwrap()
        .push((id, serde_json::from_slice(&pt).unwrap()));
    S::CREATED
}

fn rsa_service_account(dir: &std::path::Path) -> (String, Vec<u8>) {
    let key = dir.join("fcm.pem");
    let ok = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&key)
        .status()
        .expect("openssl is available")
        .success();
    assert!(ok);
    let der = dir.join("fcm.pub.der");
    let ok = Command::new("openssl")
        .args(["rsa", "-in"])
        .arg(&key)
        .args(["-RSAPublicKey_out", "-outform", "DER", "-out"])
        .arg(&der)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    (
        std::fs::read_to_string(&key).unwrap(),
        std::fs::read(&der).unwrap(),
    )
}

struct World {
    rt: Rt,
    apns: Arc<Fake>,
    fcm: Arc<Fake>,
    web: Arc<Fake>,
    web_base: String,
    web_ua: Arc<SecretKey>,
    web_auth: [u8; 16],
}

async fn world() -> Option<World> {
    let dir = std::env::temp_dir().join(format!("cf-push-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    // APNs
    let apns_key = SecretKey::random(&mut rand::rngs::OsRng);
    let apns = Arc::new(Fake::default());
    let apns_url = serve(
        Router::new()
            .route("/3/device/:token", post(apns_send))
            .with_state(ApnsFake {
                fake: apns.clone(),
                key: VerifyingKey::from(&p256::ecdsa::SigningKey::from(apns_key.clone())),
            }),
    )
    .await;
    // FCM
    let (rsa_pem, rsa_pub) = rsa_service_account(&dir);
    let fcm = Arc::new(Fake::default());
    let token_uri = Arc::new(Mutex::new(String::new()));
    let fcm_url = serve(
        Router::new()
            .route("/token", post(fcm_token))
            .route("/v1/projects/:project/:action", post(fcm_send))
            .with_state(FcmFake {
                fake: fcm.clone(),
                public_der: Arc::new(rsa_pub),
                token_uri: token_uri.clone(),
            }),
    )
    .await;
    *token_uri.lock().unwrap() = format!("{fcm_url}/token");
    // Web Push
    let ua = Arc::new(SecretKey::random(&mut rand::rngs::OsRng));
    let web_auth: [u8; 16] = rand::random();
    let web = Arc::new(Fake::default());
    let origin = Arc::new(Mutex::new(String::new()));
    let web_base = serve(
        Router::new()
            .route("/push/:id", post(web_push))
            .with_state(WebFake {
                fake: web.clone(),
                origin: origin.clone(),
                ua_secret: ua.clone(),
                auth: web_auth,
            }),
    )
    .await;
    *origin.lock().unwrap() = web_base.clone();
    let vapid = SecretKey::random(&mut rand::rngs::OsRng);
    let adapters = PushAdapters {
        apns: Some(
            Apns::new(ApnsSettings {
                team_id: "TEAMID1234".into(),
                key_id: "KEYID12345".into(),
                key_pem: apns_key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
                topic: "com.cfmeet.app".into(),
                base_url: apns_url,
            })
            .unwrap(),
        ),
        fcm: Some(
            Fcm::new(FcmSettings {
                account: ServiceAccount {
                    project_id: "cfmeet-test".into(),
                    client_email: "push@cfmeet-test.iam.gserviceaccount.com".into(),
                    private_key: rsa_pem,
                    token_uri: format!("{fcm_url}/token"),
                },
                base_url: fcm_url,
                allow_insecure: true,
            })
            .unwrap(),
        ),
        web: Some(
            WebPush::new(WebPushSettings {
                vapid_key_pem: vapid.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
                subject: "mailto:push-ops@cfmeet.test".into(),
                allowed_hosts: vec![web_base.trim_start_matches("http://").to_string()],
                allow_insecure: true,
            })
            .unwrap(),
        ),
        local: None,
    };
    let _ = std::fs::remove_dir_all(&dir);
    let rt = Rt::with_push(test_settings(), Arc::new(adapters)).await?;
    Some(World {
        rt,
        apns,
        fcm,
        web,
        web_base,
        web_ua: ua,
        web_auth,
    })
}

impl World {
    fn subscription(&self, id: &str) -> String {
        json!({ "endpoint": format!("{}/push/{id}", self.web_base), "keys": {
            "p256dh": B64URL.encode(self.web_ua.public_key().to_encoded_point(false).as_bytes()),
            "auth": B64URL.encode(self.web_auth) } })
        .to_string()
    }

    async fn register(
        &self,
        c: &Customer,
        p: Uuid,
        device: Uuid,
        platform: &str,
        handle: &str,
    ) -> Response {
        self.rt
            .h
            .put(
                c,
                Some(p),
                &format!("/v1/messaging/devices/{device}/push-registration"),
                json!({ "platform": platform, "handle": handle }),
            )
            .await
    }

    async fn dispatch(&self) -> messaging::realtime::push::PushReport {
        messaging::realtime::push::dispatch_due(&self.rt.h.state, 100, 5)
            .await
            .unwrap()
    }
}

fn metadata_only(v: &Value, conv: &str) {
    let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
    for k in &keys {
        assert!(
            ["v", "type", "conversationId", "eventSequence", "sequence"].contains(&k.as_str()),
            "unexpected key {k} in {v}"
        );
    }
    assert_eq!(v["conversationId"].as_str(), Some(conv));
}

#[tokio::test]
async fn apns_fcm_and_web_push_deliver_metadata_only_and_map_provider_answers() {
    let Some(w) = world().await else { return };
    let h = &w.rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 4).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let apns_token = "ab".repeat(32);
    let fcm_token = format!("fcm-{}", "x".repeat(60));
    let mut devices = Vec::new();
    for (p, (platform, handle)) in people[1..].iter().zip([
        ("APNS", apns_token.clone()),
        ("FCM", fcm_token.clone()),
        ("WEB_PUSH", w.subscription("sub-1")),
    ]) {
        let d = h.active_device(&c, *p).await.device_id();
        let r = w.register(&c, *p, d, platform, &handle).await;
        assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["pushDeliveryConfigured"], true, "{}", r.text);
        devices.push(d);
    }
    let _ = h.active_device(&c, people[0]).await;

    // 1. One managed message: each platform receives exactly one metadata-only push.
    let r = h
        .send_managed(&c, &conv, people[0], "the secret text")
        .await;
    assert_eq!(r.status, axum::http::StatusCode::CREATED, "{}", r.text);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let report = w.dispatch().await;
    assert_eq!((report.sent, report.failed), (3, 0), "{report:?}");
    let a = w.apns.received();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].0, apns_token);
    assert_eq!(a[0].1["aps"]["mutable-content"], 1);
    assert!(a[0].1["aps"]["alert"].get("body").is_none(), "{}", a[0].1);
    metadata_only(&a[0].1["cf"], &cid);
    let f = w.fcm.received();
    assert_eq!((f.len(), f[0].0.as_str()), (1, fcm_token.as_str()));
    metadata_only(&f[0].1, &cid);
    let wp = w.web.received();
    assert_eq!((wp.len(), wp[0].0.as_str()), (1, "sub-1"));
    metadata_only(&wp[0].1, &cid);
    for (_, v) in a.iter().chain(&f).chain(&wp) {
        assert!(!v.to_string().contains("secret"), "{v}");
    }
    assert_eq!(
        *w.apns.refused.lock().unwrap()
            + *w.fcm.refused.lock().unwrap()
            + *w.web.refused.lock().unwrap(),
        0
    );
    assert_eq!(
        w.fcm.tokens_issued.lock().unwrap().len(),
        1,
        "the OAuth token is cached"
    );

    // 2. Throttling / outages / an expired APNs provider token are retried, then sent.
    w.apns
        .script(&[(403, json!({ "reason": "ExpiredProviderToken" }))]);
    w.fcm
        .script(&[(503, json!({ "error": { "status": "UNAVAILABLE" } }))]);
    w.web.script(&[(429, Value::Null)]);
    h.send_managed(&c, &conv, people[0], "second").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let report = w.dispatch().await;
    assert_eq!((report.sent, report.retried), (0, 3), "{report:?}");
    h.clock.advance(chrono::Duration::minutes(5));
    let report = w.dispatch().await;
    assert_eq!((report.sent, report.retried), (3, 0), "{report:?}");
    assert_eq!(
        (
            w.apns.received().len(),
            w.fcm.received().len(),
            w.web.received().len()
        ),
        (2, 2, 2)
    );

    // 3. An expired FCM access token (401) is dropped and re-acquired.
    w.fcm.tokens_issued.lock().unwrap().clear();
    h.send_managed(&c, &conv, people[0], "third").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let report = w.dispatch().await;
    assert_eq!((report.sent, report.retried), (2, 1), "{report:?}");
    h.clock.advance(chrono::Duration::minutes(5));
    let report = w.dispatch().await;
    assert_eq!(report.sent, 1, "{report:?}");
    assert_eq!(w.fcm.tokens_issued.lock().unwrap().len(), 1);

    // 4. Unregistered handles disable their registration; nothing is reported as sent.
    w.apns.script(&[(410, json!({ "reason": "Unregistered" }))]);
    w.fcm.script(&[(
        404,
        json!({ "error": { "status": "NOT_FOUND", "details": [{ "errorCode": "UNREGISTERED" }] } }),
    )]);
    w.web.script(&[(410, Value::Null)]);
    h.send_managed(&c, &conv, people[0], "fourth").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let report = w.dispatch().await;
    assert_eq!((report.sent, report.failed), (0, 3), "{report:?}");
    for d in &devices {
        assert_eq!(
            h.scalar(&format!("SELECT count(*) FROM messaging_push_registrations WHERE device_id = '{d}' AND NOT enabled AND disabled_reason = 'INVALID_HANDLE'")).await,
            1
        );
    }
    h.send_managed(&c, &conv, people[0], "fifth").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let report = w.dispatch().await;
    assert_eq!(report.sent, 0, "{report:?}");
    println!(
        "EVIDENCE push_adapters apns_sent={} fcm_sent={} web_push_sent={} oauth_tokens=2 unregistered_disabled=3",
        w.apns.received().len(),
        w.fcm.received().len(),
        w.web.received().len()
    );
}

#[tokio::test]
async fn e2ee_pushes_carry_no_plaintext_and_handles_are_validated() {
    let Some(w) = world().await else { return };
    let h = &w.rt.h;
    let c = h.customer(ALL_SCOPES);
    let (conv, mut alice, bob) = h.e2ee_direct(&c).await;
    let r = w
        .register(
            &c,
            bob.principal,
            bob.device_id(),
            "WEB_PUSH",
            &w.subscription("bob"),
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
    h.send_e2ee(&c, &conv, &mut alice, "an end-to-end secret")
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(w.dispatch().await.sent, 1);
    let got = w.web.received();
    metadata_only(&got[0].1, conv["conversationId"].as_str().unwrap());
    assert!(!got[0].1.to_string().contains("secret"));

    // Registration refuses handles that are malformed or aim at non-push-service hosts.
    let d = bob.device_id();
    for (platform, handle) in [
        ("APNS", "not-a-device-token".to_string()),
        ("FCM", "short".to_string()),
        ("WEB_PUSH", "{}".to_string()),
        (
            "WEB_PUSH",
            w.subscription("x")
                .replace(&w.web_base, "https://169.254.169.254"),
        ),
        (
            "WEB_PUSH",
            w.subscription("x")
                .replace(&w.web_base, "http://10.0.0.5:80"),
        ),
    ] {
        let r = w.register(&c, bob.principal, d, platform, &handle).await;
        assert_eq!(
            r.status,
            axum::http::StatusCode::BAD_REQUEST,
            "{platform} {handle}: {}",
            r.text
        );
    }
}
