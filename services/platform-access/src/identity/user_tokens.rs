//! CF Meet user tokens (ADR 0062 §2; contract `docs/platform/build-internal-contracts.md` §1).
//!
//! Wire format: `cfut_<sbx|test|live>_<compact JWS>`, at most 2048 bytes. The JWS header is
//! EXACTLY `{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"<16 lowercase hex>"}` (byte for
//! byte); the signature is Ed25519 (RFC 8032, `ed25519-dalek`) by a Platform Access-only
//! key (`PLATFORM_ACCESS_USER_TOKEN_SIGNING_KEY_FILE`). `kid` is the first 8 bytes of the
//! SHA-256 of the raw public key, hex.
//!
//! [`UserTokenKeys::decode`] is the STATELESS part of verification: structure, exact
//! header, algorithm, known key, signature, issuer / audience, environment tag, grant
//! vocabulary and the `nbf` / `exp` window (60 s skew). [`verify_live`] adds every live
//! rule: the principal is ACTIVE and not tombstoned, `iat >= tokens_not_before`, the minting
//! credential (and its service account) still authenticates with FULL authority, the
//! tenant is EXTERNAL, every id of the token matches the persisted chain, and the
//! effective grants are the claimed grants ∩ what the credential's CURRENT scopes allow.
//! Tokens signed by a retired key are `USER_TOKEN_REVOKED`.

use std::collections::BTreeSet;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    authentication::{AuthFailureReason, AuthenticationError, AuthorityMode, CredentialAuthenticator},
    end_users::{self, EndUserRow},
    scopes::names,
};
use crate::domain::{EnvironmentKind, PrincipalKind, PrincipalStatus, TenantClassification};

pub const ISSUER: &str = "cfmeet.platform-access";
pub const AUDIENCE: &str = "cfmeet.platform.client";
pub const TOKEN_TYPE: &str = "cfmeet-user+jwt";
pub const ALGORITHM: &str = "EdDSA";
pub const PREFIX: &str = "cfut_";
/// Hard upper bound of a token, prefix included.
pub const MAX_TOKEN_BYTES: usize = 2048;
/// Allowed clock skew on `nbf` / `exp` / `iat`.
pub const SKEW_SECONDS: i64 = 60;
pub const DEFAULT_TTL_SECONDS: i64 = 3600;
pub const MIN_TTL_SECONDS: i64 = 60;
pub const MAX_TTL_SECONDS: i64 = 86_400;
/// At most this many `mtg` entries per token.
pub const MAX_MEETING_GRANTS: usize = 10;
/// Mints per user per clock hour.
pub const MINTS_PER_HOUR: i64 = 120;

pub const MESSAGE_GRANTS: &[&str] = &["read", "send"];
pub const CALL_GRANTS: &[&str] = &["receive"];
pub const MEETING_ROLES: &[&str] = &["HOST", "CO_HOST", "PRESENTER", "PARTICIPANT", "VIEWER"];

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

/// One meeting grant (wire form of requests and verify responses).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeetingGrant {
    pub conference_id: Uuid,
    pub role: String,
}

/// Validated grants. Wire form `{"msg":[…],"call":[…],"mtg":[{"conferenceId","role"}]}`
/// (every part optional in requests, always present in responses).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    #[serde(default)]
    pub msg: Vec<String>,
    #[serde(default)]
    pub call: Vec<String>,
    #[serde(default)]
    pub mtg: Vec<MeetingGrant>,
}

impl Grants {
    /// Validates and normalizes (sorted, de-duplicated) grants. The message is safe to
    /// return.
    pub fn validated(self) -> Result<Grants, String> {
        fn closed(values: Vec<String>, allowed: &[&str], part: &str) -> Result<Vec<String>, String> {
            let set: BTreeSet<String> = values.into_iter().collect();
            if let Some(bad) = set.iter().find(|v| !allowed.contains(&v.as_str())) {
                let shown: String = bad.chars().take(32).collect();
                return Err(format!(
                    "grants.{part} accepts only {allowed:?} (got {shown:?})"
                ));
            }
            Ok(set.into_iter().collect())
        }
        let msg = closed(self.msg, MESSAGE_GRANTS, "msg")?;
        let call = closed(self.call, CALL_GRANTS, "call")?;
        if self.mtg.len() > MAX_MEETING_GRANTS {
            return Err(format!(
                "grants.mtg holds at most {MAX_MEETING_GRANTS} entries"
            ));
        }
        let mut seen = BTreeSet::new();
        for g in &self.mtg {
            if !MEETING_ROLES.contains(&g.role.as_str()) {
                return Err(format!("grants.mtg[].role must be one of {MEETING_ROLES:?}"));
            }
            if !seen.insert(g.conference_id) {
                return Err("grants.mtg names a conference more than once".to_string());
            }
        }
        let mut mtg = self.mtg;
        mtg.sort();
        Ok(Grants { msg, call, mtg })
    }

    /// The credential scopes these grants require (§1 table).
    pub fn required_scopes(&self) -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        for m in &self.msg {
            out.insert(if m == "read" {
                names::MESSAGES_READ
            } else {
                names::MESSAGES_SEND
            });
        }
        if !self.call.is_empty() {
            out.insert(names::CALLS_WRITE);
        }
        if !self.mtg.is_empty() {
            out.insert(names::MEETINGS_WRITE);
        }
        out
    }

    /// The grants `scopes` (the credential's CURRENT effective scopes) still allow.
    pub fn intersect(&self, scopes: &[String]) -> Grants {
        let has = |s: &str| scopes.iter().any(|x| x == s);
        Grants {
            msg: self
                .msg
                .iter()
                .filter(|m| {
                    has(if m.as_str() == "read" {
                        names::MESSAGES_READ
                    } else {
                        names::MESSAGES_SEND
                    })
                })
                .cloned()
                .collect(),
            call: if has(names::CALLS_WRITE) {
                self.call.clone()
            } else {
                Vec::new()
            },
            mtg: if has(names::MEETINGS_WRITE) {
                self.mtg.clone()
            } else {
                Vec::new()
            },
        }
    }

    fn to_claims(&self) -> ClaimGrants {
        ClaimGrants {
            msg: (!self.msg.is_empty()).then(|| self.msg.clone()),
            call: (!self.call.is_empty()).then(|| self.call.clone()),
            mtg: (!self.mtg.is_empty()).then(|| {
                self.mtg
                    .iter()
                    .map(|g| ClaimMeetingGrant {
                        c: g.conference_id,
                        r: g.role.clone(),
                    })
                    .collect()
            }),
        }
    }

    fn from_claims(claims: ClaimGrants) -> Result<Grants, TokenRejection> {
        Grants {
            msg: claims.msg.unwrap_or_default(),
            call: claims.call.unwrap_or_default(),
            mtg: claims
                .mtg
                .unwrap_or_default()
                .into_iter()
                .map(|g| MeetingGrant {
                    conference_id: g.c,
                    role: g.r,
                })
                .collect(),
        }
        .validated()
        .map_err(|_| TokenRejection::Invalid)
    }
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimMeetingGrant {
    c: Uuid,
    r: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimGrants {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    msg: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mtg: Option<Vec<ClaimMeetingGrant>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireClaims {
    iss: String,
    aud: String,
    sub: Uuid,
    tid: Uuid,
    aid: Uuid,
    eid: Uuid,
    ekd: String,
    sva: Uuid,
    crd: Uuid,
    jti: Uuid,
    iat: i64,
    nbf: i64,
    exp: i64,
    grt: ClaimGrants,
}

/// The claims of a user token (validated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTokenClaims {
    pub principal_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: EnvironmentKind,
    pub service_account_id: Uuid,
    pub credential_id: Uuid,
    pub token_id: Uuid,
    pub issued_at: i64,
    pub not_before: i64,
    pub expires_at: i64,
    pub grants: Grants,
}

/// Why a token was refused (the 401 codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRejection {
    /// Malformed, bad signature, unknown key, wrong issuer / audience / environment, not
    /// yet valid, or describing a chain that does not exist.
    Invalid,
    /// Past `exp` (+ skew).
    Expired,
    /// Was valid, but its user, credential, scope or signing key no longer allows it.
    Revoked,
}

impl TokenRejection {
    pub fn as_str(&self) -> &'static str {
        match self {
            TokenRejection::Invalid => "USER_TOKEN_INVALID",
            TokenRejection::Expired => "USER_TOKEN_EXPIRED",
            TokenRejection::Revoked => "USER_TOKEN_REVOKED",
        }
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

fn kid_of(key: &VerifyingKey) -> String {
    hex::encode(&Sha256::digest(key.as_bytes())[..8])
}

/// A raw 32-byte Ed25519 public key, base64url without padding.
pub fn parse_public_key(value: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(value).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// A published signing key (`GET /internal/v1/user-tokens/keys`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicKeyView {
    pub kid: String,
    pub alg: &'static str,
    pub crv: &'static str,
    pub x: String,
    /// `ACTIVE` (verifies) or `RETIRED` (tokens it signed are revoked).
    pub status: &'static str,
}

/// The signing key, previous keys still accepted during a rotation, and retired keys.
pub struct UserTokenKeys {
    signing: SigningKey,
    kid: String,
    previous: Vec<(String, VerifyingKey)>,
    retired: Vec<(String, VerifyingKey)>,
}

impl std::fmt::Debug for UserTokenKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserTokenKeys")
            .field("kid", &self.kid)
            .field("previous", &self.previous.len())
            .field("retired", &self.retired.len())
            .finish_non_exhaustive()
    }
}

impl UserTokenKeys {
    pub fn new(signing: SigningKey, previous: Vec<VerifyingKey>, retired: Vec<VerifyingKey>) -> Self {
        let kid = kid_of(&signing.verifying_key());
        let retired: Vec<(String, VerifyingKey)> =
            retired.into_iter().map(|k| (kid_of(&k), k)).collect();
        Self {
            signing,
            previous: previous
                .into_iter()
                .map(|k| (kid_of(&k), k))
                .filter(|(k, _)| *k != kid && !retired.iter().any(|(r, _)| r == k))
                .collect(),
            retired: retired.into_iter().filter(|(k, _)| *k != kid).collect(),
            kid,
        }
    }

    /// A random key (tests and local development).
    pub fn generate() -> Self {
        Self::new(
            SigningKey::from_bytes(&rand::random::<[u8; 32]>()),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Parses a PKCS#8 PEM Ed25519 private key; errors never echo key material.
    pub fn from_pkcs8_pem(
        pem: &str,
        previous: Vec<VerifyingKey>,
        retired: Vec<VerifyingKey>,
    ) -> Result<Self, String> {
        use ed25519_dalek::pkcs8::DecodePrivateKey as _;
        let key = SigningKey::from_pkcs8_pem(pem)
            .map_err(|_| "is not a PKCS#8 PEM Ed25519 private key".to_string())?;
        Ok(Self::new(key, previous, retired))
    }

    #[cfg(test)]
    pub(crate) fn to_pkcs8_pem_for_tests(&self) -> String {
        use ed25519_dalek::pkcs8::{spki::der::pem::LineEnding, EncodePrivateKey as _};
        self.signing
            .to_pkcs8_pem(LineEnding::LF)
            .expect("encodes")
            .to_string()
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The raw public key of the signing key (base64url).
    pub fn public_x(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.signing.verifying_key().as_bytes())
    }

    pub fn public_keys(&self) -> Vec<PublicKeyView> {
        let view = |kid: &str, key: &VerifyingKey, status| PublicKeyView {
            kid: kid.to_string(),
            alg: ALGORITHM,
            crv: "Ed25519",
            x: URL_SAFE_NO_PAD.encode(key.as_bytes()),
            status,
        };
        let mut out = vec![view(&self.kid, &self.signing.verifying_key(), "ACTIVE")];
        out.extend(self.previous.iter().map(|(k, v)| view(k, v, "ACTIVE")));
        out.extend(self.retired.iter().map(|(k, v)| view(k, v, "RETIRED")));
        out
    }

    fn header_segment(kid: &str) -> String {
        // The exact header bytes (field order fixed by `Header`).
        URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&Header {
                alg: ALGORITHM.into(),
                typ: TOKEN_TYPE.into(),
                kid: kid.to_string(),
            })
            .expect("serializes"),
        )
    }

    /// Signs `claims` into a `cfut_` token.
    pub fn sign(&self, claims: &UserTokenClaims) -> String {
        let wire = WireClaims {
            iss: ISSUER.into(),
            aud: AUDIENCE.into(),
            sub: claims.principal_id,
            tid: claims.tenant_id,
            aid: claims.application_id,
            eid: claims.environment_id,
            ekd: claims.environment_kind.as_str().into(),
            sva: claims.service_account_id,
            crd: claims.credential_id,
            jti: claims.token_id,
            iat: claims.issued_at,
            nbf: claims.not_before,
            exp: claims.expires_at,
            grt: claims.grants.to_claims(),
        };
        let h = Self::header_segment(&self.kid);
        let c = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire).expect("serializes"));
        let input = format!("{h}.{c}");
        let sig = self.signing.sign(input.as_bytes());
        format!(
            "{PREFIX}{}_{input}.{}",
            claims.environment_kind.credential_tag(),
            URL_SAFE_NO_PAD.encode(sig.to_bytes())
        )
    }

    /// Stateless verification at `now` (see the module documentation).
    pub fn decode(&self, token: &str, now: DateTime<Utc>) -> Result<UserTokenClaims, TokenRejection> {
        use TokenRejection::*;
        if token.len() > MAX_TOKEN_BYTES || !token.is_ascii() {
            return Err(Invalid);
        }
        let rest = token.strip_prefix(PREFIX).ok_or(Invalid)?;
        let (tag, jws) = rest.split_once('_').ok_or(Invalid)?;
        let tag_kind = EnvironmentKind::from_credential_tag(tag).ok_or(Invalid)?;
        let mut parts = jws.split('.');
        let (Some(h), Some(c), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Invalid);
        };
        let header: Header = URL_SAFE_NO_PAD
            .decode(h)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(Invalid)?;
        if header.alg != ALGORITHM
            || header.typ != TOKEN_TYPE
            || header.kid.len() != 16
            || !header
                .kid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            // Exactly the canonical header: no extra whitespace, order or fields.
            || h != Self::header_segment(&header.kid)
        {
            return Err(Invalid);
        }
        let (key, retired) = if header.kid == self.kid {
            (self.signing.verifying_key(), false)
        } else if let Some((_, k)) = self.previous.iter().find(|(k, _)| *k == header.kid) {
            (*k, false)
        } else if let Some((_, k)) = self.retired.iter().find(|(k, _)| *k == header.kid) {
            (*k, true)
        } else {
            return Err(Invalid);
        };
        let sig: [u8; 64] = URL_SAFE_NO_PAD
            .decode(s)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(Invalid)?;
        key.verify_strict(
            format!("{h}.{c}").as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig),
        )
        .map_err(|_| Invalid)?;
        if retired {
            return Err(Revoked);
        }
        let wire: WireClaims = URL_SAFE_NO_PAD
            .decode(c)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(Invalid)?;
        if wire.iss != ISSUER || wire.aud != AUDIENCE {
            return Err(Invalid);
        }
        let kind = EnvironmentKind::parse(&wire.ekd).ok_or(Invalid)?;
        if kind != tag_kind {
            return Err(Invalid);
        }
        let grants = Grants::from_claims(wire.grt)?;
        let t = now.timestamp();
        if wire.exp <= wire.iat
            || wire.nbf < wire.iat
            || wire.exp - wire.iat > MAX_TTL_SECONDS
            || wire.iat > t + SKEW_SECONDS
        {
            return Err(Invalid);
        }
        if t > wire.exp + SKEW_SECONDS {
            return Err(Expired);
        }
        if wire.nbf > t + SKEW_SECONDS {
            return Err(Invalid);
        }
        Ok(UserTokenClaims {
            principal_id: wire.sub,
            tenant_id: wire.tid,
            application_id: wire.aid,
            environment_id: wire.eid,
            environment_kind: kind,
            service_account_id: wire.sva,
            credential_id: wire.crd,
            token_id: wire.jti,
            issued_at: wire.iat,
            not_before: wire.nbf,
            expires_at: wire.exp,
            grants,
        })
    }
}

// ---------------------------------------------------------------------------
// Live verification
// ---------------------------------------------------------------------------

/// A verified user token: its claims, the user and the EFFECTIVE grants.
#[derive(Debug, Clone)]
pub struct VerifiedUserToken {
    pub claims: UserTokenClaims,
    pub user: EndUserRow,
    pub effective: Grants,
}

#[derive(Debug)]
pub enum VerifyError {
    Rejected(TokenRejection),
    Unavailable(String),
}

impl From<TokenRejection> for VerifyError {
    fn from(r: TokenRejection) -> Self {
        VerifyError::Rejected(r)
    }
}

/// Full verification: [`UserTokenKeys::decode`] and every live rule.
pub async fn verify_live(
    keys: &UserTokenKeys,
    authenticator: &CredentialAuthenticator,
    db: &DatabaseConnection,
    token: &str,
    now: DateTime<Utc>,
) -> Result<VerifiedUserToken, VerifyError> {
    use TokenRejection::*;
    let claims = keys.decode(token, now)?;
    // The minting credential: revocation, expiry, supersession, suspension and the
    // service account are evaluated now, exactly as at authentication.
    let chain = match authenticator.authority_of(claims.credential_id).await {
        Ok(chain) => chain,
        Err(AuthenticationError::Rejected { reason, .. }) => {
            return Err(match reason {
                AuthFailureReason::UnknownCredential | AuthFailureReason::CorruptAuthorityChain => {
                    VerifyError::Rejected(Invalid)
                }
                _ => VerifyError::Rejected(Revoked),
            })
        }
        Err(AuthenticationError::Unavailable { summary }) => {
            return Err(VerifyError::Unavailable(summary))
        }
    };
    if chain.tenant.id != claims.tenant_id
        || chain.application.id != claims.application_id
        || chain.environment.id != claims.environment_id
        || chain.environment.kind != claims.environment_kind
        || chain.service_account.id != claims.service_account_id
        || chain.tenant.classification != TenantClassification::External
    {
        return Err(Invalid.into());
    }
    if chain.authority_mode != AuthorityMode::Full {
        return Err(Revoked.into());
    }
    let user = end_users::load(db, claims.principal_id)
        .await
        .map_err(|e| VerifyError::Unavailable(super::error::summarize_db_error(&e)))?
        .ok_or(Invalid)?;
    if user.tenant_id != claims.tenant_id
        || user.application_id != claims.application_id
        || user.environment_id != claims.environment_id
        || user.kind != PrincipalKind::ExternalApplicationPrincipal
    {
        return Err(Invalid.into());
    }
    if user.tombstoned || user.status != PrincipalStatus::Active {
        return Err(Revoked.into());
    }
    if user
        .tokens_not_before
        .is_some_and(|tnb| claims.issued_at < tnb.timestamp())
    {
        return Err(Revoked.into());
    }
    let effective = claims.grants.intersect(&chain.scopes);
    Ok(VerifiedUserToken {
        claims,
        user,
        effective,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(now: i64) -> UserTokenClaims {
        UserTokenClaims {
            principal_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            environment_kind: EnvironmentKind::Sandbox,
            service_account_id: Uuid::new_v4(),
            credential_id: Uuid::new_v4(),
            token_id: Uuid::new_v4(),
            issued_at: now,
            not_before: now,
            expires_at: now + 3600,
            grants: Grants {
                msg: vec!["read".into(), "send".into()],
                call: vec!["receive".into()],
                mtg: vec![MeetingGrant {
                    conference_id: Uuid::new_v4(),
                    role: "PARTICIPANT".into(),
                }],
            },
        }
    }

    fn segments(token: &str) -> (String, Vec<String>) {
        let (prefix, jws) = token.split_at(PREFIX.len() + 4);
        (
            prefix.to_string(),
            jws.split('.').map(str::to_string).collect(),
        )
    }

    #[test]
    fn tokens_round_trip_with_the_exact_wire_format() {
        let keys = UserTokenKeys::generate();
        let now = Utc::now();
        let c = claims(now.timestamp());
        let t = keys.sign(&c);
        assert!(t.starts_with("cfut_sbx_"));
        assert!(t.len() < MAX_TOKEN_BYTES);
        assert_eq!(keys.decode(&t, now).unwrap(), c);
        let (_, parts) = segments(&t);
        let header = String::from_utf8(URL_SAFE_NO_PAD.decode(&parts[0]).unwrap()).unwrap();
        assert_eq!(
            header,
            format!(r#"{{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"{}"}}"#, keys.kid())
        );
        assert_eq!(keys.kid().len(), 16);
        let body: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&parts[1]).unwrap()).unwrap();
        assert_eq!(body["iss"], ISSUER);
        assert_eq!(body["aud"], AUDIENCE);
        assert_eq!(body["ekd"], "SANDBOX");
        assert_eq!(body["grt"]["msg"], serde_json::json!(["read", "send"]));
        assert_eq!(body["grt"]["mtg"][0]["r"], "PARTICIPANT");
    }

    #[test]
    fn tampering_keys_algorithms_and_time_are_refused() {
        use TokenRejection::*;
        let keys = UserTokenKeys::generate();
        let now = Utc::now();
        let c = claims(now.timestamp());
        let t = keys.sign(&c);
        let (prefix, parts) = segments(&t);
        let join = |p: &[String]| format!("{prefix}{}", p.join("."));
        // Another key.
        assert_eq!(UserTokenKeys::generate().decode(&t, now), Err(Invalid));
        // Tampered signature.
        let mut bad = parts.clone();
        let mut sig = URL_SAFE_NO_PAD.decode(&bad[2]).unwrap();
        sig[0] ^= 1;
        bad[2] = URL_SAFE_NO_PAD.encode(sig);
        assert_eq!(keys.decode(&join(&bad), now), Err(Invalid));
        // Forged claims with the original signature.
        let mut forged = parts.clone();
        let mut body: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&forged[1]).unwrap()).unwrap();
        body["sub"] = serde_json::json!(Uuid::new_v4());
        forged[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&body).unwrap());
        assert_eq!(keys.decode(&join(&forged), now), Err(Invalid));
        // Another algorithm / type / kid shape / non-canonical header.
        for h in [
            format!(r#"{{"alg":"none","typ":"cfmeet-user+jwt","kid":"{}"}}"#, keys.kid()),
            format!(r#"{{"alg":"EdDSA","typ":"JWT","kid":"{}"}}"#, keys.kid()),
            r#"{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"0123"}"#.to_string(),
            format!(r#"{{"typ":"cfmeet-user+jwt","alg":"EdDSA","kid":"{}"}}"#, keys.kid()),
            format!(r#"{{"alg":"EdDSA", "typ":"cfmeet-user+jwt","kid":"{}"}}"#, keys.kid()),
            format!(
                r#"{{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"{}","x":1}}"#,
                keys.kid()
            ),
        ] {
            let mut p = parts.clone();
            p[0] = URL_SAFE_NO_PAD.encode(h.as_bytes());
            assert_eq!(keys.decode(&join(&p), now), Err(Invalid), "{h}");
        }
        // Wrong environment tag.
        assert_eq!(
            keys.decode(&t.replacen("cfut_sbx_", "cfut_live_", 1), now),
            Err(Invalid)
        );
        // Expired (past the skew) / within the skew / not yet valid.
        assert_eq!(
            keys.decode(&t, now + chrono::Duration::seconds(3600 + SKEW_SECONDS + 1)),
            Err(Expired)
        );
        assert!(keys
            .decode(&t, now + chrono::Duration::seconds(3600 + SKEW_SECONDS - 1))
            .is_ok());
        assert_eq!(
            keys.decode(&t, now - chrono::Duration::seconds(SKEW_SECONDS + 5)),
            Err(Invalid)
        );
        // Size, prefix and garbage.
        for bad in [
            "",
            "cfut_",
            "cfmk_sbx_a.b.c",
            "cfut_sbx_a.b",
            "cfut_sbx_a.b.c.d",
            &format!("cfut_sbx_{}", "x".repeat(MAX_TOKEN_BYTES)),
        ] {
            assert_eq!(keys.decode(bad, now), Err(Invalid), "{bad}");
        }
        assert!(!format!("{keys:?}").contains("signing:"));
    }

    #[test]
    fn rotation_keeps_previous_keys_and_retired_keys_revoke() {
        let now = Utc::now();
        let c = claims(now.timestamp());
        let old = UserTokenKeys::generate();
        let gone = UserTokenKeys::generate();
        let t_old = old.sign(&c);
        let t_gone = gone.sign(&c);
        let rotated = UserTokenKeys::new(
            SigningKey::from_bytes(&rand::random::<[u8; 32]>()),
            vec![old.signing.verifying_key()],
            vec![gone.signing.verifying_key()],
        );
        assert_eq!(rotated.decode(&t_old, now).unwrap(), c);
        assert_eq!(rotated.decode(&t_gone, now), Err(TokenRejection::Revoked));
        let keys = rotated.public_keys();
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].status, "ACTIVE");
        assert_eq!(keys[0].kid, rotated.kid());
        assert_eq!(keys[1].kid, old.kid());
        assert_eq!(keys[2].status, "RETIRED");
        assert_eq!(parse_public_key(&keys[2].x), Some(gone.signing.verifying_key()));
        assert_eq!(parse_public_key("not-a-key"), None);
    }

    #[test]
    fn grants_are_closed_bounded_and_intersected_with_scopes() {
        let g = Grants {
            msg: vec!["send".into(), "read".into(), "read".into()],
            call: vec!["receive".into()],
            mtg: vec![],
        }
        .validated()
        .unwrap();
        assert_eq!(g.msg, vec!["read", "send"]);
        assert_eq!(
            g.required_scopes().into_iter().collect::<Vec<_>>(),
            vec!["calls.write", "messages.read", "messages.send"]
        );
        let shrunk = g.intersect(&["messages.read".to_string()]);
        assert_eq!(shrunk.msg, vec!["read"]);
        assert!(shrunk.call.is_empty());
        for bad in [
            Grants {
                msg: vec!["write".into()],
                ..Default::default()
            },
            Grants {
                call: vec!["place".into()],
                ..Default::default()
            },
            Grants {
                mtg: vec![MeetingGrant {
                    conference_id: Uuid::new_v4(),
                    role: "OWNER".into(),
                }],
                ..Default::default()
            },
            Grants {
                mtg: (0..11)
                    .map(|_| MeetingGrant {
                        conference_id: Uuid::new_v4(),
                        role: "VIEWER".into(),
                    })
                    .collect(),
                ..Default::default()
            },
        ] {
            assert!(bad.validated().is_err());
        }
        let id = Uuid::new_v4();
        let dup = Grants {
            mtg: vec![
                MeetingGrant {
                    conference_id: id,
                    role: "HOST".into(),
                },
                MeetingGrant {
                    conference_id: id,
                    role: "VIEWER".into(),
                },
            ],
            ..Default::default()
        };
        assert!(dup.validated().is_err());
    }
}
