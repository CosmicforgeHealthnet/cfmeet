//! Messaging Client Access Tokens (Part V §59).
//!
//! Short-lived, device-bound bearer tokens a client DEVICE presents only to obtain a
//! single-use WebSocket ticket. They are issued by Messaging after Platform Access
//! authenticated the application's credential and the acting principal, and bind every
//! authoritative field server-side: issuer, audience, tenant, application, environment,
//! principal, device, session, token id, issue and expiry times, Messaging scopes.
//!
//! Format: a compact JWS (`header.claims.signature`, base64url) with exactly
//! `{"alg":"EdDSA","typ":"cfmeet-mcat+jwt","kid":<16 hex>}` and an Ed25519 signature
//! (RFC 8032, `ed25519-dalek`) by a Messaging-only key (`MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE`).
//! Verification accepts only that algorithm, a known key id, the exact issuer and
//! audience, an unexpired window (bounded skew) and well-formed ids; a token is never
//! authority by itself — the session, device and principal are re-validated on use.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const ISSUER: &str = "cfmeet.messaging";
pub const AUDIENCE: &str = "cfmeet.messaging.realtime";
pub const TOKEN_TYPE: &str = "cfmeet-mcat+jwt";
/// Allowed clock skew when checking `iat` / `exp`.
pub const SKEW_SECONDS: i64 = 30;
/// Hard upper bound of a presented token.
pub const MAX_TOKEN_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// The claims of a client access token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub iss: String,
    pub aud: String,
    /// The principal.
    pub sub: Uuid,
    pub tid: Uuid,
    pub aid: Uuid,
    pub eid: Uuid,
    /// The device.
    pub did: Uuid,
    /// The session.
    pub sid: Uuid,
    pub jti: Uuid,
    pub iat: i64,
    pub exp: i64,
    pub scp: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported token algorithm or type")]
    Algorithm,
    #[error("unknown signing key")]
    UnknownKey,
    #[error("bad signature")]
    Signature,
    #[error("wrong issuer or audience")]
    Audience,
    #[error("token expired or not yet valid")]
    Expired,
}

impl TokenError {
    pub fn as_str(&self) -> &'static str {
        match self {
            TokenError::Malformed => "MALFORMED",
            TokenError::Algorithm => "ALGORITHM",
            TokenError::UnknownKey => "UNKNOWN_KEY",
            TokenError::Signature => "SIGNATURE",
            TokenError::Audience => "AUDIENCE",
            TokenError::Expired => "EXPIRED",
        }
    }
}

fn kid_of(key: &VerifyingKey) -> String {
    hex::encode(&Sha256::digest(key.as_bytes())[..8])
}

/// Signs and verifies client access tokens (one active key; verification also accepts
/// configured previous public keys during a rotation).
pub struct ClientTokenKeys {
    signing: SigningKey,
    kid: String,
    previous: Vec<(String, VerifyingKey)>,
}

impl std::fmt::Debug for ClientTokenKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientTokenKeys")
            .field("kid", &self.kid)
            .field("previous", &self.previous.len())
            .finish_non_exhaustive()
    }
}

impl ClientTokenKeys {
    pub fn new(signing: SigningKey, previous: Vec<VerifyingKey>) -> Self {
        let kid = kid_of(&signing.verifying_key());
        Self {
            signing,
            kid,
            previous: previous.into_iter().map(|k| (kid_of(&k), k)).collect(),
        }
    }

    /// A random key (tests and local development).
    pub fn generate() -> Self {
        Self::new(
            SigningKey::from_bytes(&rand::random::<[u8; 32]>()),
            Vec::new(),
        )
    }

    /// Loads a PKCS#8 PEM Ed25519 private key file; errors never echo key material.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        use ed25519_dalek::pkcs8::DecodePrivateKey as _;
        let pem = zeroize::Zeroizing::new(
            std::fs::read_to_string(path).map_err(|_| "cannot be read".to_string())?,
        );
        let key = SigningKey::from_pkcs8_pem(&pem)
            .map_err(|_| "is not a PKCS#8 PEM Ed25519 private key".to_string())?;
        Ok(Self::new(key, Vec::new()))
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn sign(&self, claims: &Claims) -> String {
        let header = Header {
            alg: "EdDSA".into(),
            typ: TOKEN_TYPE.into(),
            kid: self.kid.clone(),
        };
        let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("serializes"));
        let c = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("serializes"));
        let input = format!("{h}.{c}");
        let sig = self.signing.sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
    }

    fn key(&self, kid: &str) -> Option<VerifyingKey> {
        if kid == self.kid {
            Some(self.signing.verifying_key())
        } else {
            self.previous
                .iter()
                .find(|(k, _)| k == kid)
                .map(|(_, v)| *v)
        }
    }

    /// Verifies signature, algorithm, key, issuer, audience and validity at `now`.
    pub fn verify(&self, token: &str, now: DateTime<Utc>) -> Result<Claims, TokenError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(TokenError::Malformed);
        }
        let mut parts = token.split('.');
        let (Some(h), Some(c), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        let header: Header = URL_SAFE_NO_PAD
            .decode(h)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(TokenError::Malformed)?;
        if header.alg != "EdDSA" || header.typ != TOKEN_TYPE {
            return Err(TokenError::Algorithm);
        }
        let key = self.key(&header.kid).ok_or(TokenError::UnknownKey)?;
        let sig: [u8; 64] = URL_SAFE_NO_PAD
            .decode(s)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(TokenError::Malformed)?;
        key.verify(
            format!("{h}.{c}").as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig),
        )
        .map_err(|_| TokenError::Signature)?;
        let claims: Claims = URL_SAFE_NO_PAD
            .decode(c)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or(TokenError::Malformed)?;
        if claims.iss != ISSUER || claims.aud != AUDIENCE {
            return Err(TokenError::Audience);
        }
        let t = now.timestamp();
        if claims.exp <= t || claims.iat > t + SKEW_SECONDS || claims.exp <= claims.iat {
            return Err(TokenError::Expired);
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(now: i64) -> Claims {
        Claims {
            iss: ISSUER.into(),
            aud: AUDIENCE.into(),
            sub: Uuid::new_v4(),
            tid: Uuid::new_v4(),
            aid: Uuid::new_v4(),
            eid: Uuid::new_v4(),
            did: Uuid::new_v4(),
            sid: Uuid::new_v4(),
            jti: Uuid::new_v4(),
            iat: now,
            exp: now + 900,
            scp: vec!["messages.read".into()],
        }
    }

    #[test]
    fn tokens_verify_only_with_the_right_key_algorithm_audience_and_time() {
        let keys = ClientTokenKeys::generate();
        let now = Utc::now();
        let c = claims(now.timestamp());
        let t = keys.sign(&c);
        assert_eq!(keys.verify(&t, now).unwrap(), c);
        // Another key.
        assert_eq!(
            ClientTokenKeys::generate().verify(&t, now),
            Err(TokenError::UnknownKey)
        );
        // Expired / not yet valid.
        assert_eq!(
            keys.verify(&t, now + chrono::Duration::seconds(901)),
            Err(TokenError::Expired)
        );
        assert_eq!(
            keys.verify(&t, now - chrono::Duration::seconds(120)),
            Err(TokenError::Expired)
        );
        // Forged claims (another principal) with the original signature.
        let mut parts: Vec<String> = t.split('.').map(str::to_string).collect();
        let mut forged = c.clone();
        forged.sub = Uuid::new_v4();
        parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        assert_eq!(
            keys.verify(&parts.join("."), now),
            Err(TokenError::Signature)
        );
        // `alg: none` and other algorithms are refused before any key lookup.
        let none = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"cfmeet-mcat+jwt","kid":"x"}"#);
        parts[0] = none;
        assert_eq!(
            keys.verify(&parts.join("."), now),
            Err(TokenError::Algorithm)
        );
        // Wrong audience, correctly signed.
        let mut other = c.clone();
        other.aud = "cfmeet.calling".into();
        assert_eq!(
            keys.verify(&keys.sign(&other), now),
            Err(TokenError::Audience)
        );
        // Garbage.
        for bad in ["", "a.b", "a.b.c.d", &"x".repeat(5000)] {
            assert!(keys.verify(bad, now).is_err());
        }
        // Rotation: a previous key still verifies.
        let old = ClientTokenKeys::generate();
        let t_old = old.sign(&c);
        let rotated = ClientTokenKeys::new(
            SigningKey::from_bytes(&rand::random::<[u8; 32]>()),
            vec![old.signing.verifying_key()],
        );
        assert_eq!(rotated.verify(&t_old, now).unwrap(), c);
        assert!(!format!("{keys:?}").contains("signing"));
    }
}
