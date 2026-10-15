//! Short-lived Call Signaling Credentials (Part C §8).
//!
//! Minted by Calling for one endpoint of one principal, at the request of the
//! application's backend (authenticated by its API credential). The application hands the
//! credential to its browser / mobile app, which never sees the API credential.
//!
//! * Format: `cfsig_` + an HS256 JWS signed with Calling's dedicated signaling key
//!   (`CALLING_SIGNALING_SIGNING_KEY`), separate from API credential verifiers, internal
//!   service-auth keys and media provider secrets. Audience `cfmeet.calling.signaling`,
//!   purpose `call-signaling`.
//! * Claims bind tenant, application, environment, principal, endpoint, the minting
//!   service account and API credential, issue / expiry time and a unique id.
//! * It authorizes ONLY that endpoint's own realtime call-signaling channel: receiving
//!   its signals, listing its actionable invitations, accepting / declining its own
//!   invitations and obtaining media credentials for its own sessions. It can never create
//!   Calls, change configuration, query other principals or perform administration.
//! * Every use re-checks persisted state: the endpoint must still be ACTIVE and its
//!   principal active, and the minting API credential (and its whole authority chain)
//!   must still be valid and hold `calls.write`.

use std::fmt;

use chrono::{DateTime, Duration, TimeZone, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    calls::command::CallScope,
    config::ConfigError,
    domain::{ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId},
    endpoints::CallEndpointId,
};

pub const TOKEN_PREFIX: &str = "cfsig_";
pub const AUDIENCE: &str = "cfmeet.calling.signaling";
pub const ISSUER: &str = "cfmeet.calling";
pub const PURPOSE: &str = "call-signaling";
const TOKEN_TYPE: &str = "cfmeet-signaling+jwt";
pub const DEFAULT_TTL_SECONDS: i64 = 600;
pub const MAX_TTL_SECONDS: i64 = 3_600;
const MIN_KEY_LEN: usize = 32;
const MAX_TOKEN_LEN: usize = 2_048;

/// The signaling signing key. No `Debug` derive.
pub struct SignalingKeys {
    key: SecretString,
    ttl: Duration,
}

impl fmt::Debug for SignalingKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignalingKeys")
            .field("key", &"[REDACTED]")
            .field("ttl", &self.ttl)
            .finish()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    jti: String,
    iat: i64,
    nbf: i64,
    exp: i64,
    pur: String,
    ten: Uuid,
    app: Uuid,
    env: Uuid,
    prn: Uuid,
    sva: Uuid,
    crd: Uuid,
}

/// What a valid signaling credential asserts (before the persisted-state re-check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalingClaims {
    pub scope: CallScope,
    pub principal_id: PrincipalId,
    pub endpoint_id: CallEndpointId,
    pub minted_by_service_account_id: ServiceAccountId,
    pub minted_by_credential_id: CredentialId,
    pub expires_at: DateTime<Utc>,
}

/// A freshly minted credential. The token is redacted from `Debug`.
pub struct MintedSignalingCredential {
    pub token: SecretString,
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for MintedSignalingCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MintedSignalingCredential")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SignalingCredentialError {
    #[error("signaling credential invalid")]
    Invalid,
}

impl SignalingKeys {
    /// `CALLING_SIGNALING_SIGNING_KEY` (required, ≥ 32 characters, no placeholder) and
    /// `CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS` (default 600, max 3600).
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let key = crate::config::secret::required_secret(
            get,
            "CALLING_SIGNALING_SIGNING_KEY",
            "CALLING_SIGNALING_SIGNING_KEY_FILE",
        )?
        .expose_secret()
        .to_string();
        let lowered = key.to_ascii_lowercase();
        if key.len() < MIN_KEY_LEN
            || [
                "changeme",
                "change-me",
                "placeholder",
                "example",
                "secret123",
                "your-",
            ]
            .iter()
            .any(|p| lowered.contains(p))
        {
            return Err(ConfigError::invalid(
                "CALLING_SIGNALING_SIGNING_KEY",
                format!("must be a generated secret of at least {MIN_KEY_LEN} characters"),
            ));
        }
        let ttl = match get("CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS") {
            None => DEFAULT_TTL_SECONDS,
            Some(v) => v
                .trim()
                .parse::<i64>()
                .ok()
                .filter(|s| (60..=MAX_TTL_SECONDS).contains(s))
                .ok_or_else(|| {
                    ConfigError::invalid(
                        "CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS",
                        format!("must be between 60 and {MAX_TTL_SECONDS}"),
                    )
                })?,
        };
        Ok(Self {
            key: SecretString::from(key),
            ttl: Duration::seconds(ttl),
        })
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Mints a credential for one endpoint of one principal.
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &self,
        scope: CallScope,
        principal_id: PrincipalId,
        endpoint_id: CallEndpointId,
        minted_by_service_account_id: ServiceAccountId,
        minted_by_credential_id: CredentialId,
        now: DateTime<Utc>,
    ) -> Result<MintedSignalingCredential, SignalingCredentialError> {
        let expires_at = now + self.ttl;
        let mut jti = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut jti);
        let claims = Claims {
            iss: ISSUER.to_string(),
            aud: AUDIENCE.to_string(),
            sub: endpoint_id.as_uuid().to_string(),
            jti: hex::encode(jti),
            iat: now.timestamp(),
            nbf: now.timestamp(),
            exp: expires_at.timestamp(),
            pur: PURPOSE.to_string(),
            ten: scope.tenant_id().as_uuid(),
            app: scope.application_id().as_uuid(),
            env: scope.environment_id().as_uuid(),
            prn: principal_id.as_uuid(),
            sva: minted_by_service_account_id.as_uuid(),
            crd: minted_by_credential_id.as_uuid(),
        };
        let mut header = Header::new(Algorithm::HS256);
        header.typ = Some(TOKEN_TYPE.to_string());
        let jws = jsonwebtoken::encode(
            &header,
            &claims,
            &EncodingKey::from_secret(self.key.expose_secret().as_bytes()),
        )
        .map_err(|_| SignalingCredentialError::Invalid)?;
        Ok(MintedSignalingCredential {
            token: SecretString::from(format!("{TOKEN_PREFIX}{jws}")),
            expires_at: Utc
                .timestamp_opt(expires_at.timestamp(), 0)
                .single()
                .unwrap_or(expires_at),
        })
    }

    /// Verifies signature, type, issuer, audience, purpose and time. Persisted state is
    /// re-checked by the caller ([`super::SignalingAuthority`]).
    pub fn verify(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> Result<SignalingClaims, SignalingCredentialError> {
        let invalid = SignalingCredentialError::Invalid;
        if token.len() > MAX_TOKEN_LEN {
            return Err(invalid);
        }
        let jws = token.strip_prefix(TOKEN_PREFIX).ok_or(invalid)?;
        let header = jsonwebtoken::decode_header(jws).map_err(|_| invalid)?;
        if header.alg != Algorithm::HS256 || header.typ.as_deref() != Some(TOKEN_TYPE) {
            return Err(invalid);
        }
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[AUDIENCE]);
        validation.set_issuer(&[ISSUER]);
        validation.set_required_spec_claims(&["exp", "nbf", "iss", "aud", "sub"]);
        // Time is checked below against the service clock only (one time authority).
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.leeway = 0;
        let claims = jsonwebtoken::decode::<Claims>(
            jws,
            &DecodingKey::from_secret(self.key.expose_secret().as_bytes()),
            &validation,
        )
        .map_err(|_| invalid)?
        .claims;
        // Explicit time checks against the service clock (testable, no leeway).
        if claims.pur != PURPOSE || claims.exp <= now.timestamp() || claims.nbf > now.timestamp() {
            return Err(invalid);
        }
        let endpoint = Uuid::parse_str(&claims.sub).map_err(|_| invalid)?;
        Ok(SignalingClaims {
            scope: CallScope::from_parts(
                TenantId::from_uuid(claims.ten),
                ApplicationId::from_uuid(claims.app),
                EnvironmentId::from_uuid(claims.env),
            ),
            principal_id: PrincipalId::from_uuid(claims.prn),
            endpoint_id: CallEndpointId::from_uuid(endpoint),
            minted_by_service_account_id: ServiceAccountId::from_uuid(claims.sva),
            minted_by_credential_id: CredentialId::from_uuid(claims.crd),
            expires_at: Utc.timestamp_opt(claims.exp, 0).single().ok_or(invalid)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "signaling-test-key-0123456789abcdefghijklmnop";

    fn keys() -> SignalingKeys {
        SignalingKeys::from_lookup(&|k: &str| {
            (k == "CALLING_SIGNALING_SIGNING_KEY").then(|| KEY.to_string())
        })
        .unwrap()
    }

    fn scope() -> CallScope {
        CallScope::from_parts(
            TenantId::from_uuid(Uuid::from_u128(1)),
            ApplicationId::from_uuid(Uuid::from_u128(2)),
            EnvironmentId::from_uuid(Uuid::from_u128(3)),
        )
    }

    #[test]
    fn round_trip_binds_scope_principal_and_endpoint() {
        let k = keys();
        let now = Utc::now();
        let minted = k
            .mint(
                scope(),
                PrincipalId::from_uuid(Uuid::from_u128(4)),
                CallEndpointId::from_uuid(Uuid::from_u128(5)),
                ServiceAccountId::from_uuid(Uuid::from_u128(6)),
                CredentialId::from_uuid(Uuid::from_u128(7)),
                now,
            )
            .unwrap();
        let token = minted.token.expose_secret().to_string();
        assert!(token.starts_with("cfsig_"));
        assert!(!format!("{minted:?}").contains(&token[10..30]));
        let claims = k.verify(&token, now).unwrap();
        assert_eq!(claims.scope, scope());
        assert_eq!(claims.endpoint_id.as_uuid(), Uuid::from_u128(5));
        assert_eq!(claims.minted_by_credential_id.as_uuid(), Uuid::from_u128(7));
        // Expired, tampered, foreign-key and unprefixed tokens are refused.
        assert!(k
            .verify(&token, now + Duration::seconds(DEFAULT_TTL_SECONDS + 1))
            .is_err());
        assert!(
            k.verify(&token, now - Duration::seconds(1)).is_err(),
            "not yet valid"
        );
        // The service clock is the only time authority (no system-time check).
        let later = now + Duration::days(3650);
        let future = {
            let m = k
                .mint(
                    scope(),
                    PrincipalId::from_uuid(Uuid::from_u128(4)),
                    CallEndpointId::from_uuid(Uuid::from_u128(5)),
                    ServiceAccountId::from_uuid(Uuid::from_u128(6)),
                    CredentialId::from_uuid(Uuid::from_u128(7)),
                    later,
                )
                .unwrap();
            m.token.expose_secret().to_string()
        };
        assert!(k.verify(&future, later).is_ok());
        assert!(k.verify(&future, now).is_err());
        let mut tampered = token.clone();
        tampered.pop();
        tampered.push(if token.ends_with('A') { 'B' } else { 'A' });
        assert!(k.verify(&tampered, now).is_err());
        assert!(k
            .verify(token.trim_start_matches(TOKEN_PREFIX), now)
            .is_err());
        let other = SignalingKeys::from_lookup(&|k: &str| {
            (k == "CALLING_SIGNALING_SIGNING_KEY")
                .then(|| "another-signaling-key-0123456789abcdefgh".to_string())
        })
        .unwrap();
        assert!(other.verify(&token, now).is_err());
        // A platform API credential is never a signaling credential.
        assert!(k
            .verify("cfmk_live_abcdefghijklmnopqrstuvwxyz", now)
            .is_err());
    }

    #[test]
    fn key_is_required_and_strong() {
        assert!(SignalingKeys::from_lookup(&|_: &str| None).is_err());
        for weak in ["short", "changeme-changeme-changeme-changeme-x"] {
            let err = SignalingKeys::from_lookup(&|k: &str| {
                (k == "CALLING_SIGNALING_SIGNING_KEY").then(|| weak.to_string())
            })
            .unwrap_err();
            assert!(!err.to_string().contains(weak));
        }
    }

    #[test]
    fn the_key_may_be_a_secret_file_never_both_never_a_fallback() {
        use crate::config::secret::tests::TempSecret;
        let file = TempSecret::new(&format!("{KEY}\n"), 0o600);
        let path = file.path();
        let get = |k: &str| match k {
            "CALLING_SIGNALING_SIGNING_KEY_FILE" => Some(path.clone()),
            "CF_RUNTIME_ENVIRONMENT" => Some("production".to_string()),
            _ => None,
        };
        let from_file = SignalingKeys::from_lookup(&get).unwrap();
        let now = Utc::now();
        let minted = keys()
            .mint(
                scope(),
                PrincipalId::from_uuid(Uuid::from_u128(4)),
                CallEndpointId::from_uuid(Uuid::from_u128(5)),
                ServiceAccountId::from_uuid(Uuid::from_u128(6)),
                CredentialId::from_uuid(Uuid::from_u128(7)),
                now,
            )
            .unwrap();
        assert!(from_file.verify(minted.token.expose_secret(), now).is_ok());
        let both = |k: &str| match k {
            "CALLING_SIGNALING_SIGNING_KEY_FILE" => Some(path.clone()),
            "CALLING_SIGNALING_SIGNING_KEY" => Some(KEY.to_string()),
            _ => None,
        };
        assert!(SignalingKeys::from_lookup(&both).is_err());
        let missing = |k: &str| {
            (k == "CALLING_SIGNALING_SIGNING_KEY_FILE").then(|| "/nonexistent/key".to_string())
        };
        assert!(SignalingKeys::from_lookup(&missing).is_err());
        // Placeholders are refused from a file exactly as inline.
        let weak = TempSecret::new("placeholder-placeholder-placeholder-x", 0o600);
        let weak_path = weak.path();
        let get = |k: &str| (k == "CALLING_SIGNALING_SIGNING_KEY_FILE").then(|| weak_path.clone());
        assert!(SignalingKeys::from_lookup(&get).is_err());
        // Production never accepts a missing key (no fallback).
        let production =
            |k: &str| (k == "CF_RUNTIME_ENVIRONMENT").then(|| "production".to_string());
        assert!(matches!(
            SignalingKeys::from_lookup(&production),
            Err(ConfigError::Missing {
                var: "CALLING_SIGNALING_SIGNING_KEY"
            })
        ));
    }
}
