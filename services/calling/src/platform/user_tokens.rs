//! CF Meet user tokens (ADR 0062, `docs/platform/build-internal-contracts.md` §1, §2.2).
//!
//! A partner's server mints a short-lived user token (`cfut_<sbx|test|live>_<JWS>`) for one
//! of its users (an EXTERNAL_APPLICATION_PRINCIPAL of an EXTERNAL tenant). A device
//! presents it as `Authorization: Bearer cfut_…`. Calling never verifies the signature
//! itself: it asks Platform Access (`POST /internal/v1/user-tokens/verify`, permission
//! `platform-user-token.verify`, Calling's own signed service token), which answers the
//! token's scope triple, the minting credential and service account, the user's profile
//! and the EFFECTIVE grants (claimed grants ∩ the minting credential's current scopes).
//!
//! A successful verification may be reused for at most 30 s ([`CACHE_TTL`], never past
//! the token's own expiry). The cache key is the SHA-256 digest of the whole token (the
//! token itself is never kept), so a forged token that copies another token's id never
//! hits the cache; each entry also records the Platform Access token id.
//!
//! Every failure to reach Platform Access is [`UserTokenError::Unavailable`]: the device
//! request then fails closed (503), never open.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    CorrelationId, OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::domain::{EnvironmentKind, PrincipalStatus, SharedClock};

/// The device-facing prefix of a CF Meet user token.
pub const TOKEN_PREFIX: &str = "cfut_";
/// Upper bound of a presented user token (contract §1).
pub const MAX_TOKEN_LEN: usize = 2_048;
/// Longest a successful verification is reused (contract §2.2: at most 30 s).
pub const CACHE_TTL: StdDuration = StdDuration::from_secs(30);
/// Platform Access verification route.
pub const VERIFY_PATH: &str = "/internal/v1/user-tokens/verify";
/// Bound of the verification cache (entries beyond it are pruned, then dropped).
const CACHE_MAX: usize = 20_000;

/// The `call` grant a device needs to receive calls.
pub const CALL_RECEIVE: &str = "receive";

/// The user a token was minted for (as Platform Access reports it now).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserTokenUser {
    pub id: Uuid,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    pub status: PrincipalStatus,
}

/// The EFFECTIVE grants of a verified token. Unknown grant families are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserTokenGrants {
    #[serde(default)]
    pub msg: Vec<String>,
    #[serde(default)]
    pub call: Vec<String>,
    #[serde(default)]
    pub mtg: Vec<serde_json::Value>,
}

impl UserTokenGrants {
    /// Whether the token may receive calls (`call:receive`).
    pub fn may_receive_calls(&self) -> bool {
        self.call.iter().any(|g| g == CALL_RECEIVE)
    }
}

/// A token Platform Access verified (contract §2.2 200 answer). Calling's own reading:
/// unknown fields are tolerated so compatible contract additions never break Calling.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedUserToken {
    pub token_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: EnvironmentKind,
    pub credential_id: Uuid,
    pub service_account_id: Uuid,
    pub user: UserTokenUser,
    #[serde(default)]
    pub grants: UserTokenGrants,
    #[serde(deserialize_with = "instant")]
    pub issued_at: DateTime<Utc>,
    #[serde(deserialize_with = "instant")]
    pub expires_at: DateTime<Utc>,
}

/// An RFC 3339 timestamp or (tolerated) integer epoch seconds.
fn instant<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Text(String),
        Seconds(i64),
    }
    match Wire::deserialize(d)? {
        Wire::Text(s) => DateTime::parse_from_rfc3339(&s)
            .map(|t| t.with_timezone(&Utc))
            .map_err(serde::de::Error::custom),
        Wire::Seconds(s) => Utc
            .timestamp_opt(s, 0)
            .single()
            .ok_or_else(|| serde::de::Error::custom("timestamp out of range")),
    }
}

/// Why a user token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UserTokenError {
    /// Platform Access refused it (`USER_TOKEN_INVALID`, `USER_TOKEN_EXPIRED`,
    /// `USER_TOKEN_REVOKED`), or it is malformed before any call. Clients see a uniform 401.
    #[error("user token rejected ({0})")]
    Rejected(String),
    /// Platform Access could not be reached or answered unexpectedly: fail closed.
    #[error("user token verification unavailable ({0})")]
    Unavailable(String),
}

/// The verification authority (Platform Access in production; a fake in tests).
#[async_trait]
pub trait UserTokenAuthority: Send + Sync + 'static {
    async fn verify(
        &self,
        token: &str,
        correlation: &CorrelationId,
    ) -> Result<VerifiedUserToken, UserTokenError>;
}

/// `POST /internal/v1/user-tokens/verify` over Calling's internal service client.
pub struct HttpUserTokenAuthority {
    client: Arc<InternalServiceClient>,
}

impl HttpUserTokenAuthority {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl UserTokenAuthority for HttpUserTokenAuthority {
    async fn verify(
        &self,
        token: &str,
        correlation: &CorrelationId,
    ) -> Result<VerifiedUserToken, UserTokenError> {
        /// Borrowed: the token is never copied into a long-lived value nor logged.
        #[derive(Serialize)]
        struct Body<'a> {
            token: &'a str,
        }
        let request = OutboundServiceRequest::continuing(ServiceId::platform_access(), correlation);
        match self
            .client
            .post_json(&request, VERIFY_PATH, &Body { token })
            .await
        {
            Ok(response) => response
                .json::<VerifiedUserToken>()
                .map_err(|e| UserTokenError::Unavailable(e.to_string())),
            Err(InternalCallError::Status {
                status: 401,
                code: Some(code),
                ..
            }) if code.starts_with("USER_TOKEN_") => Err(UserTokenError::Rejected(code)),
            Err(other) => Err(UserTokenError::Unavailable(other.to_string())),
        }
    }
}

/// The token's environment tag (`cfut_<tag>_…`).
fn environment_tag(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (tag, jws) = rest.split_once('_')?;
    (!jws.is_empty()).then_some(tag)
}

fn tag_of(kind: EnvironmentKind) -> &'static str {
    match kind {
        EnvironmentKind::Sandbox => "sbx",
        EnvironmentKind::Test => "test",
        EnvironmentKind::Production => "live",
    }
}

/// Whether `token` is shaped like a user token (prefix, known tag, bounded, printable).
pub fn is_user_token_shaped(token: &str) -> bool {
    token.len() <= MAX_TOKEN_LEN
        && token.bytes().all(|b| b.is_ascii_graphic())
        && environment_tag(token).is_some_and(|t| matches!(t, "sbx" | "test" | "live"))
}

/// Verified user tokens with the bounded positive cache.
pub struct UserTokenVerifier {
    authority: Arc<dyn UserTokenAuthority>,
    clock: SharedClock,
    cache: Mutex<HashMap<[u8; 32], (VerifiedUserToken, Instant)>>,
}

impl UserTokenVerifier {
    pub fn new(authority: Arc<dyn UserTokenAuthority>, clock: SharedClock) -> Self {
        Self {
            authority,
            clock,
            cache: Mutex::default(),
        }
    }

    /// Verifies `token` (≤ 30 s reuse of a successful answer). The answer must name the
    /// environment kind the token's tag claims, a valid lifetime, and must not have
    /// expired on Calling's clock.
    pub async fn verify(
        &self,
        token: &str,
        correlation: &CorrelationId,
    ) -> Result<VerifiedUserToken, UserTokenError> {
        if !is_user_token_shaped(token) {
            return Err(UserTokenError::Rejected("USER_TOKEN_INVALID".into()));
        }
        let key: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let now = self.clock.now();
        if let Some(hit) = self.cached(&key, now) {
            return Ok(hit);
        }
        let verified = self.authority.verify(token, correlation).await?;
        if environment_tag(token) != Some(tag_of(verified.environment_kind))
            || verified.expires_at <= verified.issued_at
        {
            tracing::warn!("user token verification answer is inconsistent with the token");
            return Err(UserTokenError::Rejected("USER_TOKEN_INVALID".into()));
        }
        if verified.expires_at <= now {
            return Err(UserTokenError::Rejected("USER_TOKEN_EXPIRED".into()));
        }
        self.remember(key, verified.clone());
        Ok(verified)
    }

    fn cached(&self, key: &[u8; 32], now: DateTime<Utc>) -> Option<VerifiedUserToken> {
        let mut cache = self.cache.lock().expect("user token cache");
        match cache.get(key) {
            Some((v, at)) if at.elapsed() < CACHE_TTL && v.expires_at > now => Some(v.clone()),
            Some(_) => {
                cache.remove(key);
                None
            }
            None => None,
        }
    }

    fn remember(&self, key: [u8; 32], verified: VerifiedUserToken) {
        let mut cache = self.cache.lock().expect("user token cache");
        if cache.len() >= CACHE_MAX {
            cache.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(key, (verified, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
        answer: Result<VerifiedUserToken, UserTokenError>,
    }

    #[async_trait]
    impl UserTokenAuthority for Counting {
        async fn verify(
            &self,
            _: &str,
            _: &CorrelationId,
        ) -> Result<VerifiedUserToken, UserTokenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn verified(kind: EnvironmentKind, ttl: i64) -> VerifiedUserToken {
        let now = Utc::now();
        VerifiedUserToken {
            token_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: kind,
            credential_id: Uuid::new_v4(),
            service_account_id: Uuid::new_v4(),
            user: UserTokenUser {
                id: Uuid::new_v4(),
                external_id: Some("u-1".into()),
                display_name: None,
                image_url: None,
                status: PrincipalStatus::Active,
            },
            grants: UserTokenGrants {
                call: vec!["receive".into()],
                ..Default::default()
            },
            issued_at: now - chrono::Duration::seconds(5),
            expires_at: now + chrono::Duration::seconds(ttl),
        }
    }

    fn verifier(answer: Result<VerifiedUserToken, UserTokenError>) -> (UserTokenVerifier, Arc<Counting>) {
        let authority = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            answer,
        });
        (
            UserTokenVerifier::new(authority.clone(), Arc::new(crate::domain::SystemClock)),
            authority,
        )
    }

    #[test]
    fn the_wire_answer_parses_with_tolerated_fields() {
        let v: VerifiedUserToken = serde_json::from_value(serde_json::json!({
            "tokenId": Uuid::new_v4(), "tenantId": Uuid::new_v4(),
            "applicationId": Uuid::new_v4(), "environmentId": Uuid::new_v4(),
            "environmentKind": "SANDBOX", "credentialId": Uuid::new_v4(),
            "serviceAccountId": Uuid::new_v4(),
            "user": {"id": Uuid::new_v4(), "externalId": "x", "displayName": null,
                     "imageUrl": null, "status": "ACTIVE"},
            "grants": {"msg": ["read"], "call": ["receive"],
                       "mtg": [{"conferenceId": Uuid::new_v4(), "role": "HOST"}]},
            "issuedAt": "2026-11-18T10:00:00Z", "expiresAt": 1_900_000_000,
            "future": true
        }))
        .unwrap();
        assert!(v.grants.may_receive_calls());
        assert_eq!(v.environment_kind, EnvironmentKind::Sandbox);
    }

    #[test]
    fn shapes() {
        assert!(is_user_token_shaped("cfut_live_a.b.c"));
        assert!(is_user_token_shaped("cfut_sbx_a.b.c"));
        assert!(!is_user_token_shaped("cfut_prod_a.b.c"));
        assert!(!is_user_token_shaped("cfut_live_"));
        assert!(!is_user_token_shaped("cfsig_a.b.c"));
        assert!(!is_user_token_shaped(&format!("cfut_live_{}", "a".repeat(2_100))));
    }

    #[tokio::test]
    async fn success_is_reused_and_answers_are_checked() {
        let (v, authority) = verifier(Ok(verified(EnvironmentKind::Production, 600)));
        let c = CorrelationId::generate();
        v.verify("cfut_live_a.b.c", &c).await.unwrap();
        v.verify("cfut_live_a.b.c", &c).await.unwrap();
        assert_eq!(authority.calls.load(Ordering::SeqCst), 1, "cached");
        v.verify("cfut_live_a.b.d", &c).await.unwrap();
        assert_eq!(authority.calls.load(Ordering::SeqCst), 2, "per token");
        // The tag must match the environment kind Platform Access reports.
        assert!(matches!(
            v.verify("cfut_sbx_a.b.c", &c).await,
            Err(UserTokenError::Rejected(_))
        ));
        // Malformed tokens never reach Platform Access.
        let before = authority.calls.load(Ordering::SeqCst);
        assert!(v.verify("cfsig_a.b.c", &c).await.is_err());
        assert_eq!(authority.calls.load(Ordering::SeqCst), before);
    }

    #[tokio::test]
    async fn refusals_and_expired_answers_are_never_cached() {
        let (v, authority) = verifier(Err(UserTokenError::Rejected("USER_TOKEN_REVOKED".into())));
        let c = CorrelationId::generate();
        assert!(v.verify("cfut_live_a.b.c", &c).await.is_err());
        assert!(v.verify("cfut_live_a.b.c", &c).await.is_err());
        assert_eq!(authority.calls.load(Ordering::SeqCst), 2);
        let (v, _) = verifier(Ok(verified(EnvironmentKind::Production, -1)));
        assert_eq!(
            v.verify("cfut_live_a.b.c", &c).await.unwrap_err(),
            UserTokenError::Rejected("USER_TOKEN_EXPIRED".into())
        );
    }
}
