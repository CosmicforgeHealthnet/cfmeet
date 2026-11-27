//! CF Meet user tokens (ADR 0062, build contract §1 / §2.2): the device credential of a
//! partner user (a Platform Access principal of an EXTERNAL application environment).
//!
//! The Meeting Service never verifies a user token's signature itself: Platform Access
//! verifies it LIVE (`POST /internal/v1/user-tokens/verify`, permission
//! `platform-user-token.verify`) and answers the user's scope, profile and EFFECTIVE
//! grants (claimed grants ∩ the minting credential's current scopes). A successful answer
//! may be cached for at most 30 s ([`CachedUserTokens`]); a refusal is never cached, so a
//! suspended user, a `revoke-sessions` call or a revoked minting key apply within that
//! bound. Platform Access unreachable is [`AuthorityUnavailable`] (the request is refused
//! with 503, fail closed).
//!
//! Only the conference JOIN accepts a user token
//! (`Authorization: Bearer cfut_…`, contract security scheme `userToken`); every other
//! route keeps its existing credentials (a joined partner user continues with the
//! participant token the join returns).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::http::Method;
use cf_service_auth::{
    client::{InternalCallError, InternalServiceClient},
    OutboundServiceRequest, ServiceId,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::authority::AuthorityUnavailable;
use crate::domain::Role;

/// Wire prefix of every CF Meet user token.
pub const USER_TOKEN_PREFIX: &str = "cfut_";
/// Contract §1: a user token is at most 2048 bytes.
pub const MAX_USER_TOKEN_LEN: usize = 2048;
/// Platform Access's live verification of a user token.
pub const VERIFY_PATH: &str = "/internal/v1/user-tokens/verify";
/// Contract §2.2: a successful verification may be cached for at most 30 s.
pub const MAX_CACHE_SECONDS: u64 = 30;
/// Bound of the verification cache (entries); expired entries are evicted first.
const MAX_CACHE_ENTRIES: usize = 10_000;

/// Whether `token` has the user-token wire shape (`cfut_<sbx|test|live>_<JWS>`, bounded).
/// Shape only: the token is verified by Platform Access.
pub fn looks_like_user_token(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(USER_TOKEN_PREFIX) else {
        return false;
    };
    let tagged = ["sbx_", "test_", "live_"]
        .iter()
        .any(|t| rest.starts_with(t) && rest.len() > t.len());
    tagged
        && token.len() <= MAX_USER_TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// A meeting grant (`grt.mtg[]`): the conference and the role the partner's server granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeetingGrant {
    pub conference_id: Uuid,
    pub role: Role,
}

/// The partner user's profile as Platform Access answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformUserProfile {
    /// The Platform Access principal id (`sub`).
    pub id: Uuid,
    pub display_name: Option<String>,
    /// `ACTIVE`, `SUSPENDED` or `DEACTIVATED`.
    pub status: String,
}

/// A user token Platform Access verified just now (or within the cache bound).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedUserToken {
    pub token_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    /// `SANDBOX`, `TEST` or `PRODUCTION`.
    pub environment_kind: String,
    /// The minting credential (`crd`).
    pub credential_id: Uuid,
    /// The minting service account (`sva`).
    pub service_account_id: Uuid,
    pub user: PlatformUserProfile,
    /// EFFECTIVE meeting grants (at most 10 per the contract; extra entries are ignored).
    pub meeting_grants: Vec<MeetingGrant>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl VerifiedUserToken {
    /// The role granted for `conference_id`, if the token grants it at all.
    pub fn meeting_grant(&self, conference_id: Uuid) -> Option<Role> {
        self.meeting_grants
            .iter()
            .filter(|g| g.conference_id == conference_id)
            .map(|g| g.role)
            .max_by_key(Role::rank)
    }

    /// Parses Platform Access's 200 answer (contract §2.2). `None`: malformed.
    pub fn from_verify_response(body: &Value) -> Option<Self> {
        let id = |key: &str| -> Option<Uuid> { Uuid::parse_str(body.get(key)?.as_str()?).ok() };
        let kind = body.get("environmentKind")?.as_str()?;
        if !matches!(kind, "SANDBOX" | "TEST" | "PRODUCTION") {
            return None;
        }
        let user = body.get("user")?;
        let status = user.get("status")?.as_str()?.to_string();
        let display_name = user
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::to_string);
        let meeting_grants = body
            .get("grants")
            .and_then(|g| g.get("mtg"))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .take(10)
                    .filter_map(|e| {
                        let conference_id =
                            Uuid::parse_str(e.get("conferenceId")?.as_str()?).ok()?;
                        // Contract §1: r ∈ {HOST, CO_HOST, PRESENTER, PARTICIPANT, VIEWER}.
                        let role = match e.get("role")?.as_str()? {
                            "HOST" => Role::Host,
                            "CO_HOST" => Role::CoHost,
                            "PRESENTER" => Role::Presenter,
                            "PARTICIPANT" => Role::Participant,
                            "VIEWER" => Role::Viewer,
                            _ => return None,
                        };
                        Some(MeetingGrant {
                            conference_id,
                            role,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            token_id: id("tokenId")?,
            tenant_id: id("tenantId")?,
            application_id: id("applicationId")?,
            environment_id: id("environmentId")?,
            environment_kind: kind.to_string(),
            credential_id: id("credentialId")?,
            service_account_id: id("serviceAccountId")?,
            user: PlatformUserProfile {
                id: Uuid::parse_str(user.get("id")?.as_str()?).ok()?,
                display_name,
                status,
            },
            meeting_grants,
            expires_at: body.get("expiresAt").and_then(parse_instant),
        })
    }
}

fn parse_instant(v: &Value) -> Option<DateTime<Utc>> {
    match v {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        Value::Number(n) => n
            .as_i64()
            .and_then(|secs| DateTime::<Utc>::from_timestamp(secs, 0)),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserTokenVerification {
    Valid(Arc<VerifiedUserToken>),
    /// Refused by Platform Access (`USER_TOKEN_INVALID` / `_EXPIRED` / `_REVOKED`), or a
    /// user who is not ACTIVE. The code is for operator logs only.
    Rejected {
        code: String,
    },
}

#[async_trait]
pub trait UserTokenVerifier: Send + Sync {
    async fn verify(&self, token: &str) -> Result<UserTokenVerification, AuthorityUnavailable>;
}

pub type SharedUserTokenVerifier = Arc<dyn UserTokenVerifier>;

/// Platform Access over authenticated internal HTTP.
pub struct PlatformAccessUserTokens {
    client: Arc<InternalServiceClient>,
}

impl PlatformAccessUserTokens {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

fn bounded_code(v: &str) -> String {
    v.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

/// Maps Platform Access's answer (contract §2.2).
pub fn verification_of(
    status: u16,
    body: &Value,
) -> Result<UserTokenVerification, AuthorityUnavailable> {
    match status {
        200 => {
            let token = VerifiedUserToken::from_verify_response(body)
                .ok_or_else(|| AuthorityUnavailable("malformed user token verification".into()))?;
            if token.user.status != "ACTIVE" {
                return Ok(UserTokenVerification::Rejected {
                    code: "USER_NOT_ACTIVE".into(),
                });
            }
            Ok(UserTokenVerification::Valid(Arc::new(token)))
        }
        // 401 USER_TOKEN_INVALID / _EXPIRED / _REVOKED; a 4xx validation answer is a
        // refusal too (never a reason to fail open).
        400..=499 if status != 429 => Ok(UserTokenVerification::Rejected {
            code: bounded_code(
                body.pointer("/error/code")
                    .and_then(Value::as_str)
                    .unwrap_or("USER_TOKEN_INVALID"),
            ),
        }),
        other => Err(AuthorityUnavailable(format!("HTTP {other}"))),
    }
}

#[async_trait]
impl UserTokenVerifier for PlatformAccessUserTokens {
    async fn verify(&self, token: &str) -> Result<UserTokenVerification, AuthorityUnavailable> {
        let request = OutboundServiceRequest::new_operation(ServiceId::platform_access());
        let response = self
            .client
            .exchange_json(
                &request,
                Method::POST,
                VERIFY_PATH,
                Some(&json!({ "token": token })),
            )
            .await
            .map_err(|e: InternalCallError| AuthorityUnavailable(e.to_string()))?;
        let body: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body())
                .map_err(|_| AuthorityUnavailable("invalid verification body".into()))?
        };
        verification_of(response.status(), &body)
    }
}

struct CacheEntry {
    until: Instant,
    token: Arc<VerifiedUserToken>,
}

/// A positive cache in front of a verifier (contract §2.2: at most 30 s). Keyed by the
/// SHA-256 of the presented token (one entry per token, i.e. per `tokenId`); a refusal or
/// an unavailable authority is never cached, and an entry never outlives the token.
pub struct CachedUserTokens {
    inner: SharedUserTokenVerifier,
    ttl: Duration,
    entries: Mutex<HashMap<[u8; 32], CacheEntry>>,
}

impl CachedUserTokens {
    /// `ttl` is clamped to [`MAX_CACHE_SECONDS`]; zero disables caching.
    pub fn new(inner: SharedUserTokenVerifier, ttl: Duration) -> Self {
        Self {
            inner,
            ttl: ttl.min(Duration::from_secs(MAX_CACHE_SECONDS)),
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn key(token: &str) -> [u8; 32] {
        Sha256::digest(token.as_bytes()).into()
    }
}

#[async_trait]
impl UserTokenVerifier for CachedUserTokens {
    async fn verify(&self, token: &str) -> Result<UserTokenVerification, AuthorityUnavailable> {
        let key = Self::key(token);
        let now = Instant::now();
        if !self.ttl.is_zero() {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(e) = entries.get(&key).filter(|e| e.until > now) {
                if e.token.expires_at.map(|t| t > Utc::now()).unwrap_or(true) {
                    return Ok(UserTokenVerification::Valid(e.token.clone()));
                }
            }
        }
        let answer = self.inner.verify(token).await?;
        if self.ttl.is_zero() {
            return Ok(answer);
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match &answer {
            UserTokenVerification::Valid(t) => {
                if entries.len() >= MAX_CACHE_ENTRIES {
                    entries.retain(|_, e| e.until > now);
                    if entries.len() >= MAX_CACHE_ENTRIES {
                        entries.clear();
                    }
                }
                entries.insert(
                    key,
                    CacheEntry {
                        until: now + self.ttl,
                        token: t.clone(),
                    },
                );
            }
            UserTokenVerification::Rejected { .. } => {
                entries.remove(&key);
            }
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn answer(status: &str) -> Value {
        let id = || Uuid::new_v4().to_string();
        json!({
            "tokenId": id(), "tenantId": id(), "applicationId": id(), "environmentId": id(),
            "environmentKind": "SANDBOX", "credentialId": id(), "serviceAccountId": id(),
            "user": { "id": id(), "externalId": "rider-42", "displayName": "Rider", "imageUrl": null, "status": status },
            "grants": { "msg": ["read"], "call": [], "mtg": [
                { "conferenceId": id(), "role": "PRESENTER" },
                { "conferenceId": id(), "role": "OWNER" },
                { "conferenceId": "nope", "role": "HOST" },
            ]},
            "issuedAt": "2026-11-18T10:00:00Z", "expiresAt": "2099-11-18T11:00:00Z",
        })
    }

    #[test]
    fn the_wire_shape_is_recognised_by_prefix_and_tag() {
        assert!(looks_like_user_token(
            "cfut_sbx_eyJhbGciOiJFZERTQSJ9.e30.c2ln"
        ));
        assert!(looks_like_user_token("cfut_live_a.b.c"));
        assert!(!looks_like_user_token("cfut_prod_a.b.c"));
        assert!(!looks_like_user_token("cfut_sbx_"));
        assert!(!looks_like_user_token("eyJhbGciOi.e30.sig"));
        assert!(!looks_like_user_token("cfut_test_a b"));
        assert!(!looks_like_user_token(&format!(
            "cfut_test_{}",
            "a".repeat(2048)
        )));
    }

    #[test]
    fn verification_answers_map_to_effective_grants_and_refusals() {
        let body = answer("ACTIVE");
        let UserTokenVerification::Valid(t) = verification_of(200, &body).unwrap() else {
            panic!("valid");
        };
        // OWNER is never grantable; malformed entries are dropped.
        assert_eq!(t.meeting_grants.len(), 1);
        assert_eq!(t.meeting_grants[0].role, Role::Presenter);
        assert_eq!(t.user.display_name.as_deref(), Some("Rider"));
        assert!(t.expires_at.is_some());
        assert_eq!(
            verification_of(200, &answer("SUSPENDED")).unwrap(),
            UserTokenVerification::Rejected {
                code: "USER_NOT_ACTIVE".into()
            }
        );
        assert_eq!(
            verification_of(401, &json!({"error": {"code": "USER_TOKEN_REVOKED"}})).unwrap(),
            UserTokenVerification::Rejected {
                code: "USER_TOKEN_REVOKED".into()
            }
        );
        assert!(verification_of(503, &Value::Null).is_err());
        assert!(verification_of(429, &Value::Null).is_err());
        assert!(verification_of(200, &json!({"tokenId": "x"})).is_err());
    }

    struct Counting(AtomicUsize, Mutex<UserTokenVerification>);

    #[async_trait]
    impl UserTokenVerifier for Counting {
        async fn verify(&self, _: &str) -> Result<UserTokenVerification, AuthorityUnavailable> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(self.1.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn only_successes_are_cached_and_never_beyond_thirty_seconds() {
        let UserTokenVerification::Valid(t) = verification_of(200, &answer("ACTIVE")).unwrap()
        else {
            panic!()
        };
        let inner = Arc::new(Counting(
            AtomicUsize::new(0),
            Mutex::new(UserTokenVerification::Valid(t)),
        ));
        let cache = CachedUserTokens::new(inner.clone(), Duration::from_secs(3600));
        assert_eq!(cache.ttl, Duration::from_secs(MAX_CACHE_SECONDS));
        cache.verify("cfut_sbx_a.b.c").await.unwrap();
        cache.verify("cfut_sbx_a.b.c").await.unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 1, "served from the cache");
        cache.verify("cfut_sbx_other.b.c").await.unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 2);

        let refused = Arc::new(Counting(
            AtomicUsize::new(0),
            Mutex::new(UserTokenVerification::Rejected {
                code: "USER_TOKEN_REVOKED".into(),
            }),
        ));
        let cache = CachedUserTokens::new(refused.clone(), Duration::from_secs(30));
        cache.verify("cfut_sbx_a.b.c").await.unwrap();
        cache.verify("cfut_sbx_a.b.c").await.unwrap();
        assert_eq!(
            refused.0.load(Ordering::SeqCst),
            2,
            "refusals are never cached"
        );
    }
}
