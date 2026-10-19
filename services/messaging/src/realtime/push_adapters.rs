//! Live push adapters (§68): Apple Push Notification service (token-based), Firebase Cloud
//! Messaging (HTTP v1) and Web Push (RFC 8030, message encryption RFC 8291, VAPID
//! RFC 8292).
//!
//! * What leaves Messaging is METADATA ONLY — the job payload's `v`, `type`,
//!   `conversationId`, `eventSequence` and `sequence` and nothing else ([`metadata`]
//!   refuses any other key). Never plaintext, ciphertext or keys: for PRIVATE_E2EE the
//!   woken client fetches and decrypts the message itself. APNs shows a generic,
//!   client-localised alert (`loc-key`, `mutable-content` for a notification service
//!   extension); FCM sends a data-only message; Web Push encrypts the metadata to the
//!   browser's subscription keys.
//! * Credentials (APNs `.p8` key, FCM service account, VAPID key) are loaded once at
//!   startup and never logged. Provider tokens are cached and refreshed: APNs every 40
//!   minutes (Apple accepts 20–60), FCM OAuth access tokens until a minute before expiry,
//!   and both are dropped at once when the provider reports them expired or invalid.
//! * Signing uses constant-time implementations: ES256 with RustCrypto `p256`, RS256 with
//!   `ring` — never the `rsa` crate (RUSTSEC-2023-0071 stays inapplicable).
//! * Web Push endpoints are client-supplied URLs: only HTTPS endpoints whose host is on
//!   the configured allowlist of push services are accepted (at registration AND at send
//!   time) and HTTP redirects are never followed — the adapter cannot be aimed at an
//!   internal address. Plain-HTTP loopback endpoints exist only in local / test runtimes.
//! * Every provider answer is mapped to a truthful outcome: an unregistered handle
//!   disables the registration, throttling and outages are retried with backoff, and
//!   nothing is ever reported as sent without the provider's acceptance.
//! * Live certification needs real Apple / Google / browser-vendor accounts; the tests
//!   run these adapters against deterministic local fakes that verify the signatures,
//!   the token exchange and the encryption exactly as the real services specify them.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use aes_gcm::{aead::Aead, Aes128Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use hmac::{Hmac, Mac};
use p256::{
    ecdsa::{signature::Signer, Signature, SigningKey},
    elliptic_curve::sec1::ToEncodedPoint,
    pkcs8::DecodePrivateKey,
    PublicKey, SecretKey,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;

use super::push::{LocalTestPush, PushOutcome, PushProvider};

/// The only payload keys a notification may carry.
pub const METADATA_KEYS: &[&str] = &["v", "type", "conversationId", "eventSequence", "sequence"];

/// How long a notification may wait at the provider (matches the job expiry).
pub const TTL_SECONDS: u64 = 86_400;

/// The default allowlist of Web Push services (`*.` = any subdomain).
pub const DEFAULT_WEB_PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "*.push.services.mozilla.com",
    "*.notify.windows.com",
    "web.push.apple.com",
    "*.push.apple.com",
];

const APNS_PRODUCTION: &str = "https://api.push.apple.com";
const APNS_SANDBOX: &str = "https://api.sandbox.push.apple.com";
const FCM_BASE: &str = "https://fcm.googleapis.com";
const FCM_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const APNS_TOKEN_REFRESH_SECONDS: i64 = 40 * 60;

/// The metadata of a job payload as provider-safe string pairs; `None` when the payload
/// carries anything but [`METADATA_KEYS`] (then nothing is sent).
pub fn metadata(payload: &Value) -> Option<BTreeMap<String, String>> {
    let obj = payload.as_object()?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        if !METADATA_KEYS.contains(&k.as_str()) {
            return None;
        }
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        out.insert(k.clone(), s);
    }
    Some(out)
}

fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn b64json(v: &Value) -> String {
    B64URL.encode(serde_json::to_vec(v).expect("serializes"))
}

/// A compact ES256 JWS (fixed-size `r || s` signature).
pub fn es256_jwt(key: &SigningKey, header: &Value, claims: &Value) -> String {
    let input = format!("{}.{}", b64json(header), b64json(claims));
    let sig: Signature = key.sign(input.as_bytes());
    format!("{input}.{}", B64URL.encode(sig.to_bytes()))
}

fn http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

/// Strips a PEM armour (any label) and decodes the DER body.
fn pem_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("-----"))
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| "not a PEM document".to_string())
}

// ---------------------------------------------------------------------------------------
// APNs
// ---------------------------------------------------------------------------------------

pub struct ApnsSettings {
    pub team_id: String,
    pub key_id: String,
    /// The `.p8` signing key (PKCS#8 PEM, P-256).
    pub key_pem: String,
    /// The app's bundle id (`apns-topic`).
    pub topic: String,
    pub base_url: String,
}

pub struct Apns {
    settings: ApnsSettings,
    key: SigningKey,
    http: reqwest::Client,
    token: Mutex<Option<(String, i64)>>,
}

impl Apns {
    pub fn new(settings: ApnsSettings) -> Result<Self, String> {
        let secret = SecretKey::from_pkcs8_pem(&settings.key_pem)
            .map_err(|_| "the APNs key is not a PKCS#8 P-256 private key".to_string())?;
        if settings.team_id.len() != 10 || settings.key_id.len() != 10 {
            return Err("APNs team id and key id are 10 characters".into());
        }
        Ok(Self {
            key: SigningKey::from(secret),
            http: http_client(Duration::from_secs(10)),
            token: Mutex::new(None),
            settings,
        })
    }

    pub fn production_url(sandbox: bool) -> &'static str {
        if sandbox {
            APNS_SANDBOX
        } else {
            APNS_PRODUCTION
        }
    }

    fn provider_token(&self) -> String {
        let now = now_unix();
        let mut guard = self.token.lock().unwrap();
        if let Some((t, at)) = guard.as_ref() {
            if now - at < APNS_TOKEN_REFRESH_SECONDS {
                return t.clone();
            }
        }
        let t = es256_jwt(
            &self.key,
            &json!({ "alg": "ES256", "kid": self.settings.key_id }),
            &json!({ "iss": self.settings.team_id, "iat": now }),
        );
        *guard = Some((t.clone(), now));
        t
    }

    fn drop_token(&self) {
        *self.token.lock().unwrap() = None;
    }

    pub fn valid_handle(handle: &str) -> bool {
        (64..=200).contains(&handle.len())
            && handle.len().is_multiple_of(2)
            && handle.bytes().all(|b| b.is_ascii_hexdigit())
    }

    async fn send(&self, handle: &str, meta: &BTreeMap<String, String>) -> PushOutcome {
        if !Self::valid_handle(handle) {
            return PushOutcome::InvalidHandle;
        }
        let body = json!({
            "aps": { "alert": { "loc-key": "CF_MESSAGE_NOTIFICATION" }, "mutable-content": 1, "sound": "default" },
            "cf": meta,
        });
        let r = self
            .http
            .post(format!(
                "{}/3/device/{handle}",
                self.settings.base_url.trim_end_matches('/')
            ))
            .header("authorization", format!("bearer {}", self.provider_token()))
            .header("apns-topic", &self.settings.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header(
                "apns-expiration",
                (now_unix() + TTL_SECONDS as i64).to_string(),
            )
            .json(&body)
            .send()
            .await;
        let r = match r {
            Ok(r) => r,
            Err(e) => return PushOutcome::Retryable(format!("apns transport: {}", kind(&e))),
        };
        let status = r.status().as_u16();
        let reason = r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["reason"].as_str().map(str::to_string))
            .unwrap_or_default();
        match (status, reason.as_str()) {
            (200, _) => PushOutcome::Sent,
            (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => {
                PushOutcome::InvalidHandle
            }
            (403, "ExpiredProviderToken" | "InvalidProviderToken") => {
                self.drop_token();
                PushOutcome::Retryable(format!("apns {status} {reason}"))
            }
            (429 | 500 | 503, _) => PushOutcome::Retryable(format!("apns {status} {reason}")),
            _ => PushOutcome::Permanent(format!("apns {status} {reason}")),
        }
    }
}

fn kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "request"
    }
}

// ---------------------------------------------------------------------------------------
// FCM (HTTP v1)
// ---------------------------------------------------------------------------------------

/// The fields of a Google service-account key file this adapter uses.
#[derive(Deserialize)]
pub struct ServiceAccount {
    pub project_id: String,
    pub client_email: String,
    pub private_key: String,
    #[serde(default = "default_token_uri")]
    pub token_uri: String,
}

fn default_token_uri() -> String {
    FCM_TOKEN_URI.into()
}

pub struct FcmSettings {
    pub account: ServiceAccount,
    pub base_url: String,
    /// Plain-HTTP token / API endpoints are refused unless this is set (local / test).
    pub allow_insecure: bool,
}

pub struct Fcm {
    project_id: String,
    client_email: String,
    token_uri: String,
    base_url: String,
    key: ring::signature::RsaKeyPair,
    rng: ring::rand::SystemRandom,
    http: reqwest::Client,
    access: tokio::sync::Mutex<Option<(String, i64)>>,
}

impl Fcm {
    pub fn new(s: FcmSettings) -> Result<Self, String> {
        let der = pem_der(&s.account.private_key)?;
        let key = ring::signature::RsaKeyPair::from_pkcs8(&der)
            .map_err(|_| "the FCM service-account key is not a PKCS#8 RSA key".to_string())?;
        for u in [&s.account.token_uri, &s.base_url] {
            if !(u.starts_with("https://") || (s.allow_insecure && u.starts_with("http://"))) {
                return Err("FCM endpoints must be HTTPS".into());
            }
        }
        if !s.allow_insecure && s.account.token_uri != FCM_TOKEN_URI {
            return Err("the FCM token endpoint must be Google's OAuth endpoint".into());
        }
        if s.account.project_id.is_empty()
            || !s
                .account
                .project_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid FCM project id".into());
        }
        Ok(Self {
            project_id: s.account.project_id,
            client_email: s.account.client_email,
            token_uri: s.account.token_uri,
            base_url: s.base_url,
            key,
            rng: ring::rand::SystemRandom::new(),
            http: http_client(Duration::from_secs(10)),
            access: tokio::sync::Mutex::new(None),
        })
    }

    pub fn default_base_url() -> &'static str {
        FCM_BASE
    }

    /// The RS256 assertion exchanged for an OAuth access token (RFC 7523).
    fn assertion(&self, now: i64) -> Result<String, String> {
        let input = format!(
            "{}.{}",
            b64json(&json!({ "alg": "RS256", "typ": "JWT" })),
            b64json(
                &json!({ "iss": self.client_email, "scope": FCM_SCOPE, "aud": self.token_uri,
                "iat": now, "exp": now + 3600 })
            )
        );
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &self.rng,
                input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| "signing failed".to_string())?;
        Ok(format!("{input}.{}", B64URL.encode(sig)))
    }

    async fn access_token(&self) -> Result<String, PushOutcome> {
        let now = now_unix();
        let mut guard = self.access.lock().await;
        if let Some((t, exp)) = guard.as_ref() {
            if *exp > now + 60 {
                return Ok(t.clone());
            }
        }
        let assertion = self.assertion(now).map_err(PushOutcome::Permanent)?;
        // JWS characters are URL-safe; only the grant type needs escaping.
        let body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
        );
        let r = self
            .http
            .post(&self.token_uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| PushOutcome::Retryable(format!("fcm token transport: {}", kind(&e))))?;
        let status = r.status().as_u16();
        if status != 200 {
            return Err(if status >= 500 || status == 429 {
                PushOutcome::Retryable(format!("fcm token {status}"))
            } else {
                PushOutcome::Permanent(format!("fcm token refused {status}"))
            });
        }
        let v: Value = r
            .json()
            .await
            .map_err(|_| PushOutcome::Retryable("fcm token: malformed answer".into()))?;
        let (Some(token), Some(expires_in)) =
            (v["access_token"].as_str(), v["expires_in"].as_i64())
        else {
            return Err(PushOutcome::Retryable("fcm token: malformed answer".into()));
        };
        *guard = Some((token.to_string(), now + expires_in));
        Ok(token.to_string())
    }

    pub fn valid_handle(handle: &str) -> bool {
        (32..=4096).contains(&handle.len())
            && handle
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b))
    }

    async fn send(&self, handle: &str, meta: &BTreeMap<String, String>) -> PushOutcome {
        if !Self::valid_handle(handle) {
            return PushOutcome::InvalidHandle;
        }
        let token = match self.access_token().await {
            Ok(t) => t,
            Err(o) => return o,
        };
        let body = json!({ "message": {
            "token": handle,
            "data": meta,
            "android": { "priority": "HIGH", "ttl": format!("{TTL_SECONDS}s") },
        }});
        let r = self
            .http
            .post(format!(
                "{}/v1/projects/{}/messages:send",
                self.base_url.trim_end_matches('/'),
                self.project_id
            ))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await;
        let r = match r {
            Ok(r) => r,
            Err(e) => return PushOutcome::Retryable(format!("fcm transport: {}", kind(&e))),
        };
        let status = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        let code = v["error"]["details"]
            .as_array()
            .and_then(|d| d.iter().find_map(|x| x["errorCode"].as_str()))
            .or_else(|| v["error"]["status"].as_str())
            .unwrap_or("")
            .to_string();
        match (status, code.as_str()) {
            (200, _) => PushOutcome::Sent,
            (404, _) | (_, "UNREGISTERED") => PushOutcome::InvalidHandle,
            (401, _) => {
                *self.access.lock().await = None;
                PushOutcome::Retryable("fcm 401".into())
            }
            (429 | 500 | 503, _) => PushOutcome::Retryable(format!("fcm {status} {code}")),
            _ => PushOutcome::Permanent(format!("fcm {status} {code}")),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Web Push
// ---------------------------------------------------------------------------------------

/// A browser `PushSubscription` (the registration handle, as JSON).
#[derive(Debug, Clone, Deserialize)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

pub struct ParsedSubscription {
    pub endpoint: reqwest::Url,
    pub ua_public: PublicKey,
    pub auth: [u8; 16],
}

/// Whether `host` matches an allowlist entry (`*.example.com` matches subdomains only).
pub fn host_allowed(host: &str, allowed: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|rule| {
        let rule = rule.to_ascii_lowercase();
        match rule.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
            None => host == rule,
        }
    })
}

/// Parses and checks a subscription: HTTPS (plain HTTP only to loopback when
/// `allow_insecure`), an allowlisted push-service host, no credentials in the URL, a valid
/// P-256 public key and a 16-byte auth secret.
pub fn parse_subscription(
    handle: &str,
    allowed: &[String],
    allow_insecure: bool,
) -> Result<ParsedSubscription, &'static str> {
    let s: Subscription =
        serde_json::from_str(handle).map_err(|_| "a Web Push handle is a PushSubscription JSON")?;
    let url = reqwest::Url::parse(&s.endpoint).map_err(|_| "invalid push endpoint")?;
    let host = url.host_str().ok_or("invalid push endpoint")?;
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    match url.scheme() {
        "https" => {}
        "http" if allow_insecure && loopback => {}
        _ => return Err("the push endpoint must be HTTPS"),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the push endpoint must not carry credentials");
    }
    let authority = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    };
    if !host_allowed(host, allowed) && !host_allowed(&authority, allowed) {
        return Err("the push endpoint is not an allowed push service");
    }
    let ua = B64URL
        .decode(s.keys.p256dh.trim_end_matches('='))
        .map_err(|_| "invalid p256dh key")?;
    let ua_public = PublicKey::from_sec1_bytes(&ua).map_err(|_| "invalid p256dh key")?;
    if ua.len() != 65 {
        return Err("p256dh must be an uncompressed P-256 point");
    }
    let auth: [u8; 16] = B64URL
        .decode(s.keys.auth.trim_end_matches('='))
        .ok()
        .and_then(|a| a.try_into().ok())
        .ok_or("auth must be 16 bytes")?;
    Ok(ParsedSubscription {
        endpoint: url,
        ua_public,
        auth,
    })
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// RFC 8291 §3.4 / RFC 8188 `aes128gcm` encryption of ONE record (deterministic in
/// `as_secret` and `salt`, so the RFC 8291 Appendix A vector can be checked exactly).
pub fn encrypt_aes128gcm(
    plaintext: &[u8],
    ua_public: &PublicKey,
    auth: &[u8; 16],
    as_secret: &SecretKey,
    salt: &[u8; 16],
) -> Vec<u8> {
    let ua_bytes = ua_public.to_encoded_point(false);
    let as_bytes = as_secret.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua_public.as_affine());
    // PRK_key = HMAC(auth, ecdh); IKM = HMAC(PRK_key, "WebPush: info\0" || ua || as || 0x01)
    let prk_key = hmac_sha256(auth, &[&shared.raw_secret_bytes()[..]]);
    let ikm = hmac_sha256(
        &prk_key,
        &[
            b"WebPush: info\0",
            ua_bytes.as_bytes(),
            as_bytes.as_bytes(),
            &[1],
        ],
    );
    let prk = hmac_sha256(salt, &[&ikm]);
    let cek = hmac_sha256(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]]);
    let nonce = hmac_sha256(&prk, &[b"Content-Encoding: nonce\0", &[1]]);
    let mut record = plaintext.to_vec();
    record.push(2); // last-record delimiter, no padding
    let cipher = Aes128Gcm::new_from_slice(&cek[..16]).expect("16-byte key");
    let ct = cipher
        .encrypt(
            &Nonce::from(<[u8; 12]>::try_from(&nonce[..12]).expect("12 bytes")),
            record.as_slice(),
        )
        .expect("encrypts");
    let rs: u32 = 4096;
    let mut out = Vec::with_capacity(16 + 4 + 1 + 65 + ct.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&rs.to_be_bytes());
    out.push(65);
    out.extend_from_slice(as_bytes.as_bytes());
    out.extend_from_slice(&ct);
    out
}

pub struct WebPushSettings {
    /// The VAPID application-server key (PKCS#8 PEM, P-256).
    pub vapid_key_pem: String,
    /// `mailto:` or `https:` contact (RFC 8292 `sub`).
    pub subject: String,
    pub allowed_hosts: Vec<String>,
    pub allow_insecure: bool,
}

pub struct WebPush {
    vapid: SigningKey,
    vapid_public: String,
    subject: String,
    allowed_hosts: Vec<String>,
    allow_insecure: bool,
    http: reqwest::Client,
}

impl WebPush {
    pub fn new(s: WebPushSettings) -> Result<Self, String> {
        let secret = SecretKey::from_pkcs8_pem(&s.vapid_key_pem)
            .map_err(|_| "the VAPID key is not a PKCS#8 P-256 private key".to_string())?;
        if !(s.subject.starts_with("mailto:") || s.subject.starts_with("https://")) {
            return Err("the VAPID subject is a mailto: or https: contact".into());
        }
        if s.allowed_hosts.is_empty() {
            return Err("the Web Push host allowlist is empty".into());
        }
        let vapid_public = B64URL.encode(secret.public_key().to_encoded_point(false).as_bytes());
        Ok(Self {
            vapid: SigningKey::from(secret),
            vapid_public,
            subject: s.subject,
            allowed_hosts: s.allowed_hosts,
            allow_insecure: s.allow_insecure,
            http: http_client(Duration::from_secs(10)),
        })
    }

    /// The public application-server key clients pass to `pushManager.subscribe`.
    pub fn application_server_key(&self) -> &str {
        &self.vapid_public
    }

    pub fn check(&self, handle: &str) -> Result<ParsedSubscription, &'static str> {
        parse_subscription(handle, &self.allowed_hosts, self.allow_insecure)
    }

    async fn send(&self, handle: &str, meta: &BTreeMap<String, String>) -> PushOutcome {
        let sub = match self.check(handle) {
            Ok(s) => s,
            Err(_) => return PushOutcome::InvalidHandle,
        };
        let audience = sub.endpoint.origin().ascii_serialization();
        let jwt = es256_jwt(
            &self.vapid,
            &json!({ "typ": "JWT", "alg": "ES256" }),
            &json!({ "aud": audience, "exp": now_unix() + 12 * 3600, "sub": self.subject }),
        );
        let as_secret = SecretKey::random(&mut rand::rngs::OsRng);
        let salt: [u8; 16] = rand::random();
        let body = encrypt_aes128gcm(
            &serde_json::to_vec(meta).expect("serializes"),
            &sub.ua_public,
            &sub.auth,
            &as_secret,
            &salt,
        );
        let r = self
            .http
            .post(sub.endpoint)
            .header(
                "authorization",
                format!("vapid t={jwt}, k={}", self.vapid_public),
            )
            .header("ttl", TTL_SECONDS.to_string())
            .header("content-encoding", "aes128gcm")
            .header("content-type", "application/octet-stream")
            .header("urgency", "high")
            .body(body)
            .send()
            .await;
        let r = match r {
            Ok(r) => r,
            Err(e) => return PushOutcome::Retryable(format!("web push transport: {}", kind(&e))),
        };
        match r.status().as_u16() {
            200..=202 => PushOutcome::Sent,
            404 | 410 => PushOutcome::InvalidHandle,
            s @ (429 | 500..=599) => PushOutcome::Retryable(format!("web push {s}")),
            s => PushOutcome::Permanent(format!("web push {s}")),
        }
    }
}

// ---------------------------------------------------------------------------------------
// The router over the configured adapters
// ---------------------------------------------------------------------------------------

/// Every configured adapter; a platform without one is `NOT_CONFIGURED`.
#[derive(Default)]
pub struct PushAdapters {
    pub apns: Option<Apns>,
    pub fcm: Option<Fcm>,
    pub web: Option<WebPush>,
    pub local: Option<Arc<LocalTestPush>>,
}

impl PushAdapters {
    /// Builds every configured adapter, reading and checking its key material (a bad key
    /// or file refuses startup; nothing is logged about the secret itself).
    pub fn build(
        cfg: &crate::config::PushAdapterConfig,
        allow_insecure: bool,
        local: Option<Arc<LocalTestPush>>,
    ) -> Result<Self, String> {
        let read = |p: &std::path::Path, what: &str| {
            std::fs::read_to_string(p).map_err(|_| format!("cannot read the {what} file"))
        };
        let apns = match &cfg.apns {
            None => None,
            Some(a) => Some(Apns::new(ApnsSettings {
                team_id: a.team_id.clone(),
                key_id: a.key_id.clone(),
                key_pem: read(&a.key_file, "APNs key")?,
                topic: a.topic.clone(),
                base_url: a.base_url.clone(),
            })?),
        };
        let fcm = match &cfg.fcm {
            None => None,
            Some(f) => {
                let account: ServiceAccount =
                    serde_json::from_str(&read(&f.service_account_file, "FCM service account")?)
                        .map_err(|_| {
                            "the FCM service-account file is not a service-account key".to_string()
                        })?;
                Some(Fcm::new(FcmSettings {
                    account,
                    base_url: f.base_url.clone(),
                    allow_insecure,
                })?)
            }
        };
        let web = match &cfg.web {
            None => None,
            Some(w) => Some(WebPush::new(WebPushSettings {
                vapid_key_pem: read(&w.vapid_key_file, "VAPID key")?,
                subject: w.subject.clone(),
                allowed_hosts: w.allowed_hosts.clone(),
                allow_insecure,
            })?),
        };
        Ok(Self {
            apns,
            fcm,
            web,
            local,
        })
    }

    /// The configured platforms (logged at startup; never credentials).
    pub fn configured(&self) -> Vec<&'static str> {
        ["APNS", "FCM", "WEB_PUSH", "LOCAL_TEST"]
            .into_iter()
            .filter(|p| self.supports(p))
            .collect()
    }
}

/// Structural handle checks applied at registration whatever is configured.
pub fn validate_handle_shape(platform: &str, handle: &str) -> Result<(), &'static str> {
    match platform {
        "APNS" if !Apns::valid_handle(handle) => {
            Err("an APNs device token is 64-200 hex characters")
        }
        "FCM" if !Fcm::valid_handle(handle) => Err("invalid FCM registration token"),
        "WEB_PUSH" => {
            let s: Subscription = serde_json::from_str(handle)
                .map_err(|_| "a Web Push handle is a PushSubscription JSON")?;
            if !(s.endpoint.starts_with("https://") || s.endpoint.starts_with("http://")) {
                return Err("invalid push endpoint");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[async_trait::async_trait]
impl PushProvider for PushAdapters {
    fn name(&self) -> &'static str {
        "adapters"
    }

    fn supports(&self, platform: &str) -> bool {
        match platform {
            "APNS" => self.apns.is_some(),
            "FCM" => self.fcm.is_some(),
            "WEB_PUSH" => self.web.is_some(),
            "LOCAL_TEST" => self.local.is_some(),
            _ => false,
        }
    }

    fn validate_handle(&self, platform: &str, handle: &str) -> Result<(), &'static str> {
        validate_handle_shape(platform, handle)?;
        match (platform, &self.web) {
            ("WEB_PUSH", Some(w)) => w.check(handle).map(|_| ()),
            _ => Ok(()),
        }
    }

    async fn send(&self, platform: &str, handle: &str, payload: &Value) -> PushOutcome {
        if platform == "LOCAL_TEST" {
            return match &self.local {
                Some(l) => l.send(platform, handle, payload).await,
                None => PushOutcome::Permanent("not configured".into()),
            };
        }
        let Some(meta) = metadata(payload) else {
            return PushOutcome::Permanent("payload is not metadata-only".into());
        };
        match platform {
            "APNS" => match &self.apns {
                Some(a) => a.send(handle, &meta).await,
                None => PushOutcome::Permanent("not configured".into()),
            },
            "FCM" => match &self.fcm {
                Some(f) => f.send(handle, &meta).await,
                None => PushOutcome::Permanent("not configured".into()),
            },
            "WEB_PUSH" => match &self.web {
                Some(w) => w.send(handle, &meta).await,
                None => PushOutcome::Permanent("not configured".into()),
            },
            _ => PushOutcome::Permanent("unknown platform".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_metadata_leaves_messaging() {
        let ok = json!({ "v": 1, "type": "message", "conversationId": "c", "eventSequence": 4, "sequence": 3 });
        assert_eq!(metadata(&ok).unwrap().len(), 5);
        for bad in [
            json!({ "v": 1, "text": "hello" }),
            json!({ "v": 1, "ciphertext": "AAAA" }),
            json!({ "v": { "nested": true } }),
        ] {
            assert!(metadata(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn host_allowlist_matches_exact_hosts_and_subdomains_only() {
        let allowed: Vec<String> = DEFAULT_WEB_PUSH_HOSTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(host_allowed("fcm.googleapis.com", &allowed));
        assert!(host_allowed("updates.push.services.mozilla.com", &allowed));
        assert!(host_allowed("wns2-par02p.notify.windows.com", &allowed));
        assert!(
            !host_allowed("push.services.mozilla.com", &allowed),
            "wildcard needs a subdomain"
        );
        assert!(!host_allowed(
            "evil-fcm.googleapis.com.attacker.example",
            &allowed
        ));
        assert!(!host_allowed("169.254.169.254", &allowed));
        assert!(!host_allowed("xpush.apple.com", &allowed));
    }

    fn sub(endpoint: &str) -> String {
        json!({ "endpoint": endpoint, "keys": {
            "p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "auth": "BTBZMqHH6r4Tts7J_aSIgg" } })
        .to_string()
    }

    #[test]
    fn subscriptions_cannot_aim_the_server_at_internal_addresses() {
        let allowed: Vec<String> = DEFAULT_WEB_PUSH_HOSTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(parse_subscription(
            &sub("https://fcm.googleapis.com/fcm/send/abc"),
            &allowed,
            false
        )
        .is_ok());
        for bad in [
            "http://fcm.googleapis.com/fcm/send/abc",
            "https://169.254.169.254/latest/meta-data",
            "https://internal.service.local/x",
            "https://user:pw@fcm.googleapis.com/x",
            "http://127.0.0.1:8080/push",
            "file:///etc/passwd",
        ] {
            assert!(
                parse_subscription(&sub(bad), &allowed, false).is_err(),
                "{bad}"
            );
        }
        // Loopback HTTP only when explicitly allowlisted in a local / test runtime.
        let local = vec!["127.0.0.1:8080".to_string()];
        assert!(parse_subscription(&sub("http://127.0.0.1:8080/push"), &local, true).is_ok());
        assert!(parse_subscription(&sub("http://127.0.0.1:8080/push"), &local, false).is_err());
    }

    /// RFC 8291 Appendix A, byte for byte.
    #[test]
    fn rfc8291_appendix_a_vector() {
        let d = |s: &str| B64URL.decode(s).unwrap();
        let plaintext = b"When I grow up, I want to be a watermelon";
        let as_secret =
            SecretKey::from_slice(&d("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let ua_public = PublicKey::from_sec1_bytes(&d(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        ))
        .unwrap();
        let auth: [u8; 16] = d("BTBZMqHH6r4Tts7J_aSIgg").try_into().unwrap();
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_aes128gcm(plaintext, &ua_public, &auth, &as_secret, &salt);
        assert_eq!(
            B64URL.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn es256_tokens_verify_with_the_public_key() {
        use p256::ecdsa::{signature::Verifier, VerifyingKey};
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let t = es256_jwt(&key, &json!({ "alg": "ES256" }), &json!({ "iss": "x" }));
        let (input, sig) = t.rsplit_once('.').unwrap();
        let sig = Signature::from_slice(&B64URL.decode(sig).unwrap()).unwrap();
        VerifyingKey::from(&key)
            .verify(input.as_bytes(), &sig)
            .unwrap();
    }
}
