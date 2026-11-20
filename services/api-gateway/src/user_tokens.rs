//! Stateless pre-check of CF Meet user tokens (ADR 0062, build contract §1 / §9).
//!
//! On routes whose authentication modes include `USER_TOKEN`
//! (`API_GATEWAY_USER_TOKENS_ENABLED`), a device presents `Authorization: Bearer cfut_…`.
//! The gateway verifies ONLY what it can check without state:
//!
//! * the wire format `cfut_<sbx|test|live>_<compact JWS>` (at most 2048 bytes), the tag
//!   matching the `ekd` claim;
//! * the JWS header, exactly `{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"<16 hex>"}`;
//! * the Ed25519 signature, with an ACTIVE key from Platform Access
//!   (`GET /internal/v1/user-tokens/keys`, permission `platform-user-token.verify`);
//! * `iss`, `aud`, `nbf`, `exp` (60 s clock skew).
//!
//! It then forwards `Authorization` unchanged: the owning service ALWAYS does the live
//! verification (principal, credential, environment, revocation epoch, effective grants).
//! The pre-check exists so forged and expired tokens never reach a service, and so the
//! partner-website CORS decision ([`crate::origins`]) can rely on a signed `eid`.
//!
//! Keys are refreshed in the background at least every 5 minutes, on demand when a
//! token names an unknown `kid` (at most once per 30 s), and are dropped when they could
//! not be refreshed for 15 minutes. Without usable keys the gateway fails CLOSED: no
//! user token is forwarded. The token is never logged or stored.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use uuid::Uuid;

pub const USER_TOKEN_PREFIX: &str = "cfut_";
pub const MAX_USER_TOKEN_BYTES: usize = 2048;
pub const ISSUER: &str = "cfmeet.platform-access";
pub const AUDIENCE: &str = "cfmeet.platform.client";
pub const TOKEN_TYPE: &str = "cfmeet-user+jwt";
/// Allowed clock skew on `nbf` / `exp` (contract §1).
pub const SKEW_SECONDS: i64 = 60;
/// Maximum lifetime of a user token (contract §1: 60–86400 s).
pub const MAX_LIFETIME_SECONDS: i64 = 86_400;
pub const KEYS_PATH: &str = "/internal/v1/user-tokens/keys";

/// Background refresh period of the signing keys (contract §9: at most 5 minutes).
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(300);
/// Minimum time between two on-demand refreshes (unknown `kid`, stale keys).
pub const DEFAULT_MIN_REFETCH_INTERVAL: Duration = Duration::from_secs(30);
/// Keys not refreshed for this long are no longer used (fail closed).
pub const DEFAULT_MAX_STALENESS: Duration = Duration::from_secs(900);

/// One signing key as Platform Access publishes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PublishedKey {
    pub kid: String,
    #[serde(default)]
    pub alg: String,
    #[serde(default)]
    pub crv: String,
    pub x: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct KeysResponse {
    keys: Vec<PublishedKey>,
}

#[derive(Debug, thiserror::Error)]
#[error("the user-token key source is unavailable: {0}")]
pub struct KeySourceUnavailable(pub String);

/// Where the signing keys come from (Platform Access; scripted in tests).
#[async_trait::async_trait]
pub trait UserTokenKeySource: Send + Sync {
    async fn fetch_keys(&self) -> Result<Vec<PublishedKey>, KeySourceUnavailable>;
}

/// Platform Access over authenticated internal HTTP (`platform-user-token.verify`).
pub struct PlatformAccessUserTokenKeys {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessUserTokenKeys {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl UserTokenKeySource for PlatformAccessUserTokenKeys {
    async fn fetch_keys(&self) -> Result<Vec<PublishedKey>, KeySourceUnavailable> {
        let request = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        let response = self
            .client
            .get(&request, KEYS_PATH)
            .await
            .map_err(|e| KeySourceUnavailable(e.to_string()))?;
        response
            .json::<KeysResponse>()
            .map(|r| r.keys)
            .map_err(|_| KeySourceUnavailable("malformed key set".into()))
    }
}

/// Why a presented user token is refused before forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UserTokenError {
    #[error("malformed user token")]
    Malformed,
    #[error("unknown signing key")]
    UnknownKey,
    #[error("bad signature")]
    Signature,
    #[error("wrong issuer, audience or environment tag")]
    Claims,
    #[error("user token expired or not yet valid")]
    Expired,
    /// No usable signing keys (Platform Access never answered, or not for too long).
    #[error("user-token signing keys unavailable")]
    KeysUnavailable,
}

impl UserTokenError {
    /// Stable reason for logs and metrics (never the token).
    pub fn as_str(&self) -> &'static str {
        match self {
            UserTokenError::Malformed => "MALFORMED",
            UserTokenError::UnknownKey => "UNKNOWN_KEY",
            UserTokenError::Signature => "SIGNATURE",
            UserTokenError::Claims => "CLAIMS",
            UserTokenError::Expired => "EXPIRED",
            UserTokenError::KeysUnavailable => "KEYS_UNAVAILABLE",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// The claims the gateway reads (grants are evaluated by the owning service).
#[derive(Debug, Clone, Deserialize)]
struct RawClaims {
    iss: String,
    aud: String,
    sub: String,
    tid: Uuid,
    aid: Uuid,
    eid: Uuid,
    ekd: String,
    iat: i64,
    nbf: i64,
    exp: i64,
}

/// What a user token that passed the stateless pre-check asserts. NOT authority: the
/// owning service verifies the token live; the gateway uses `environment_id` only for
/// the partner-website CORS decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedUserToken {
    pub principal: String,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub expires_at: i64,
}

fn valid_kid(kid: &str) -> bool {
    kid.len() == 16
        && kid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn environment_tag(kind: &str) -> Option<&'static str> {
    match kind {
        "SANDBOX" => Some("sbx"),
        "TEST" => Some("test"),
        "PRODUCTION" => Some("live"),
        _ => None,
    }
}

/// Splits `cfut_<tag>_<header>.<claims>.<signature>` (no cryptography).
fn split(token: &str) -> Result<(&str, &str, [&str; 3]), UserTokenError> {
    if token.len() > MAX_USER_TOKEN_BYTES {
        return Err(UserTokenError::Malformed);
    }
    let rest = token
        .strip_prefix(USER_TOKEN_PREFIX)
        .ok_or(UserTokenError::Malformed)?;
    let (tag, jws) = rest.split_once('_').ok_or(UserTokenError::Malformed)?;
    if !matches!(tag, "sbx" | "test" | "live") {
        return Err(UserTokenError::Malformed);
    }
    let mut parts = jws.split('.');
    let (Some(h), Some(c), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(UserTokenError::Malformed);
    };
    if [h, c, s].iter().any(|p| {
        p.is_empty()
            || !p
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        return Err(UserTokenError::Malformed);
    }
    Ok((tag, jws, [h, c, s]))
}

fn b64(part: &str) -> Result<Vec<u8>, UserTokenError> {
    URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| UserTokenError::Malformed)
}

/// Decodes a published key: ACTIVE, EdDSA / Ed25519, a 16-hex `kid`, a valid point.
fn decode_key(k: &PublishedKey) -> Option<(String, VerifyingKey)> {
    if k.status != "ACTIVE" || k.alg != "EdDSA" || k.crv != "Ed25519" || !valid_kid(&k.kid) {
        return None;
    }
    let raw: [u8; 32] = URL_SAFE_NO_PAD.decode(&k.x).ok()?.try_into().ok()?;
    let key = VerifyingKey::from_bytes(&raw).ok()?;
    (!key.is_weak()).then(|| (k.kid.clone(), key))
}

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, VerifyingKey>,
    /// Last successful fetch.
    fetched_at: Option<Instant>,
    /// Last fetch attempt (successful or not).
    attempted_at: Option<Instant>,
}

/// Verifies user tokens with Platform Access's published signing keys.
pub struct UserTokenVerifier {
    source: Arc<dyn UserTokenKeySource>,
    cache: RwLock<KeyCache>,
    /// Serializes fetches (one in flight).
    fetching: tokio::sync::Mutex<()>,
    refresh_interval: Duration,
    min_refetch_interval: Duration,
    max_staleness: Duration,
}

impl UserTokenVerifier {
    pub fn new(source: Arc<dyn UserTokenKeySource>) -> Self {
        Self {
            source,
            cache: RwLock::new(KeyCache::default()),
            fetching: tokio::sync::Mutex::new(()),
            refresh_interval: DEFAULT_REFRESH_INTERVAL,
            min_refetch_interval: DEFAULT_MIN_REFETCH_INTERVAL,
            max_staleness: DEFAULT_MAX_STALENESS,
        }
    }

    /// Overrides the refresh timings (tests). `refresh` is capped at 5 minutes.
    pub fn with_timings(mut self, refresh: Duration, min_refetch: Duration) -> Self {
        self.refresh_interval = refresh.min(DEFAULT_REFRESH_INTERVAL);
        self.min_refetch_interval = min_refetch;
        self
    }

    /// Number of usable (ACTIVE, fresh enough) keys.
    pub fn usable_keys(&self) -> usize {
        let cache = self.cache.read().expect("key cache lock");
        if self.fresh_enough(&cache) {
            cache.keys.len()
        } else {
            0
        }
    }

    fn fresh_enough(&self, cache: &KeyCache) -> bool {
        cache
            .fetched_at
            .is_some_and(|t| t.elapsed() <= self.max_staleness)
    }

    /// Fetches the key set now. `true` when Platform Access answered.
    pub async fn refresh(&self) -> bool {
        let _guard = self.fetching.lock().await;
        self.fetch_locked().await
    }

    async fn fetch_locked(&self) -> bool {
        self.cache.write().expect("key cache lock").attempted_at = Some(Instant::now());
        match self.source.fetch_keys().await {
            Ok(published) => {
                let keys: HashMap<_, _> = published.iter().filter_map(decode_key).collect();
                if keys.is_empty() {
                    tracing::warn!("Platform Access publishes no usable ACTIVE user-token key");
                }
                let mut cache = self.cache.write().expect("key cache lock");
                cache.keys = keys;
                cache.fetched_at = Some(Instant::now());
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "user-token signing keys could not be refreshed");
                false
            }
        }
    }

    /// Fetches unless another fetch happened within the minimum interval (`force`:
    /// unknown `kid`) or the keys are still within the refresh interval.
    async fn refetch_if_due(&self, force: bool) {
        let due = |cache: &KeyCache| {
            let attempt_ok = cache
                .attempted_at
                .is_none_or(|t| t.elapsed() >= self.min_refetch_interval);
            let stale = cache
                .fetched_at
                .is_none_or(|t| t.elapsed() >= self.refresh_interval);
            attempt_ok && (force || stale)
        };
        if !due(&self.cache.read().expect("key cache lock")) {
            return;
        }
        let _guard = self.fetching.lock().await;
        // Another request may have fetched while this one waited.
        if due(&self.cache.read().expect("key cache lock")) {
            self.fetch_locked().await;
        }
    }

    /// Background refresh (every `refresh_interval`; retried sooner after a failure).
    pub async fn run_refresher(self: Arc<Self>) {
        loop {
            let ok = self.refresh().await;
            let next = if ok {
                self.refresh_interval
            } else {
                self.refresh_interval.min(Duration::from_secs(10))
            };
            tokio::time::sleep(next.max(Duration::from_millis(100))).await;
        }
    }

    fn key(&self, kid: &str) -> Result<Option<VerifyingKey>, UserTokenError> {
        let cache = self.cache.read().expect("key cache lock");
        if !self.fresh_enough(&cache) {
            return Err(UserTokenError::KeysUnavailable);
        }
        Ok(cache.keys.get(kid).copied())
    }

    /// The stateless pre-check at `now` (Unix seconds).
    pub async fn verify(&self, token: &str, now: i64) -> Result<VerifiedUserToken, UserTokenError> {
        let (tag, jws, [h, c, s]) = split(token)?;
        let header: Header =
            serde_json::from_slice(&b64(h)?).map_err(|_| UserTokenError::Malformed)?;
        if header.alg != "EdDSA" || header.typ != TOKEN_TYPE || !valid_kid(&header.kid) {
            return Err(UserTokenError::Malformed);
        }
        let signature = Signature::from_slice(&b64(s)?).map_err(|_| UserTokenError::Malformed)?;
        self.refetch_if_due(false).await;
        let key = match self.key(&header.kid)? {
            Some(k) => k,
            None => {
                // A key rotated in since the last refresh (rate-limited refetch).
                self.refetch_if_due(true).await;
                self.key(&header.kid)?.ok_or(UserTokenError::UnknownKey)?
            }
        };
        // The JWS signing input: `<header>.<claims>`, exactly as presented.
        let signing_input = &jws[..h.len() + 1 + c.len()];
        key.verify_strict(signing_input.as_bytes(), &signature)
            .map_err(|_| UserTokenError::Signature)?;
        let claims: RawClaims =
            serde_json::from_slice(&b64(c)?).map_err(|_| UserTokenError::Malformed)?;
        if claims.iss != ISSUER
            || claims.aud != AUDIENCE
            || claims.sub.is_empty()
            || environment_tag(&claims.ekd) != Some(tag)
        {
            return Err(UserTokenError::Claims);
        }
        if claims.exp <= claims.iat || claims.exp - claims.iat > MAX_LIFETIME_SECONDS {
            return Err(UserTokenError::Claims);
        }
        if now >= claims.exp + SKEW_SECONDS {
            return Err(UserTokenError::Expired);
        }
        if now + SKEW_SECONDS < claims.nbf || now + SKEW_SECONDS < claims.iat {
            return Err(UserTokenError::Expired);
        }
        Ok(VerifiedUserToken {
            principal: claims.sub,
            tenant_id: claims.tid,
            application_id: claims.aid,
            environment_id: claims.eid,
            expires_at: claims.exp,
        })
    }
}

/// The current Unix time in seconds.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
