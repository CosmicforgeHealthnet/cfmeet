//! Contact Vault delegated access (Part D §31): a short-lived credential that lets a
//! customer backend send a raw telephone number DIRECTLY to Privacy Relay's Contact Vault,
//! so the number never passes through Calling.
//!
//! Calling authenticates the customer's ordinary platform credential, checks its
//! `privacy.contacts.*` scopes and mints a Contact Vault access token; the customer then
//! calls Privacy Relay with `X-CF-Contact-Authorization: Bearer <token>`.
//!
//! It is cryptographically SEPARATE from every other credential:
//!
//! * its own JWS type `cf-contact-access+jwt` (a service token, `cf-service+jwt`, is
//!   refused here and a contact token is refused by the service-token verifier);
//! * its own Ed25519 signing key (never the service-auth key; both sides refuse a key
//!   that is trusted for service tokens too);
//! * its own header (`X-CF-Contact-Authorization`; a request also carrying a customer
//!   `Authorization` header is refused);
//! * its own trust bundle (contract `cfmeet.privacy-relay.contact-delegation-trust`): the
//!   receiver decides which issuers may delegate which contact scopes;
//! * a closed claim set with `use` = `CONTACT_VAULT_ACCESS`, one audience, the runtime
//!   environment, the delegated platform scope, the granted contact scopes and a lifetime
//!   of at most 300 s (default 120 s).

use std::{collections::BTreeMap, collections::BTreeSet, fmt, path::Path};

use http::{header::AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use jsonwebtoken::{errors::ErrorKind, Algorithm, DecodingKey, Header, Validation};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::{
    claims::{
        parse_protected_header_of, split_compact, HeaderProblem, MAX_CLOCK_SKEW_SECONDS,
        MAX_TTL_SECONDS, MIN_TTL_SECONDS,
    },
    clock::SharedClock,
    delegated::DelegatedPlatformContext,
    headers::bearer_token,
    identity::{bounded, RuntimeEnvironment, ServiceId},
    ids::{KeyId, TokenId},
    keys::{decoding_key, KeyError, ServiceSigningKey},
};

/// JWS `typ` of a Contact Vault access token.
pub const CONTACT_TOKEN_TYPE: &str = "cf-contact-access+jwt";
/// The only accepted `use` claim.
pub const CONTACT_TOKEN_USE: &str = "CONTACT_VAULT_ACCESS";
/// Claims contract major version.
pub const CONTACT_CLAIMS_VERSION: u8 = 1;
/// Default lifetime.
pub const DEFAULT_CONTACT_TTL_SECONDS: u64 = 120;
/// `X-CF-Contact-Authorization: Bearer <contact access token>`.
pub const CONTACT_AUTHORIZATION: HeaderName = HeaderName::from_static("x-cf-contact-authorization");
/// Trust bundle format version.
pub const CONTACT_TRUST_BUNDLE_VERSION: u32 = 1;
const MAX_KEYS_PER_ISSUER: usize = 4;
const MAX_ISSUERS: usize = 8;

/// A Contact Vault scope (the public `privacy.contacts.*` scopes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ContactScope {
    #[serde(rename = "privacy.contacts.read")]
    Read,
    #[serde(rename = "privacy.contacts.write")]
    Write,
    #[serde(rename = "privacy.contacts.verify")]
    Verify,
}

impl ContactScope {
    pub const ALL: [ContactScope; 3] = [
        ContactScope::Read,
        ContactScope::Write,
        ContactScope::Verify,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ContactScope::Read => "privacy.contacts.read",
            ContactScope::Write => "privacy.contacts.write",
            ContactScope::Verify => "privacy.contacts.verify",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

impl fmt::Display for ContactScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Closed claim set (contract `cfmeet.privacy-relay.contact-delegation`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactDelegationClaims {
    ver: u8,
    #[serde(rename = "use")]
    token_use: String,
    iss: ServiceId,
    aud: ServiceId,
    runtime_environment: RuntimeEnvironment,
    iat: i64,
    nbf: i64,
    exp: i64,
    jti: TokenId,
    delegated_context: DelegatedPlatformContext,
    scopes: Vec<ContactScope>,
}

impl ContactDelegationClaims {
    /// Parses claims JSON (contract conformance only; parsing establishes no trust).
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContactSignError {
    #[error("contact access token lifetime must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS} seconds")]
    InvalidTtl,
    #[error("a contact access token needs at least one scope")]
    NoScopes,
    #[error("a contact access token cannot be addressed to its issuer")]
    SelfAudience,
    #[error("token signing failed")]
    Signing,
}

/// A minted token. `Debug` is redacted; there is no `Display` / `Serialize`.
pub struct ContactAccessToken {
    token: SecretString,
    expires_at: i64,
    token_id: TokenId,
    scopes: Vec<ContactScope>,
}

impl ContactAccessToken {
    /// The compact JWS, to hand to the customer exactly once.
    pub fn expose_for_delivery(&self) -> &str {
        self.token.expose_secret()
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn token_id(&self) -> &TokenId {
        &self.token_id
    }

    pub fn scopes(&self) -> &[ContactScope] {
        &self.scopes
    }
}

impl fmt::Debug for ContactAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactAccessToken")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Mints contact access tokens with the issuer's dedicated contact delegation key.
pub struct ContactDelegationSigner {
    issuer: ServiceId,
    runtime_environment: RuntimeEnvironment,
    key: ServiceSigningKey,
    ttl: u64,
    clock: SharedClock,
}

impl fmt::Debug for ContactDelegationSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactDelegationSigner")
            .field("issuer", &self.issuer)
            .field("kid", self.key.kid())
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl ContactDelegationSigner {
    pub fn new(
        issuer: ServiceId,
        runtime_environment: RuntimeEnvironment,
        key: ServiceSigningKey,
        ttl_seconds: u64,
        clock: SharedClock,
    ) -> Result<Self, ContactSignError> {
        if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl_seconds) {
            return Err(ContactSignError::InvalidTtl);
        }
        Ok(Self {
            issuer,
            runtime_environment,
            key,
            ttl: ttl_seconds,
            clock,
        })
    }

    pub fn issuer(&self) -> &ServiceId {
        &self.issuer
    }

    pub fn key_id(&self) -> &KeyId {
        self.key.kid()
    }

    /// Public key of the delegation key (published in the receiver's contact trust bundle).
    pub fn public_jwk(&self) -> crate::keys::PublicJwk {
        self.key.public_jwk()
    }

    pub fn ttl_seconds(&self) -> u64 {
        self.ttl
    }

    /// A token for `audience` acting for `delegated` with exactly `scopes`.
    pub fn sign(
        &self,
        audience: &ServiceId,
        delegated: &DelegatedPlatformContext,
        scopes: &[ContactScope],
    ) -> Result<ContactAccessToken, ContactSignError> {
        if audience == &self.issuer {
            return Err(ContactSignError::SelfAudience);
        }
        let scopes: Vec<ContactScope> = scopes
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if scopes.is_empty() {
            return Err(ContactSignError::NoScopes);
        }
        let now = self.clock.now_unix();
        let jti = TokenId::generate();
        let claims = ContactDelegationClaims {
            ver: CONTACT_CLAIMS_VERSION,
            token_use: CONTACT_TOKEN_USE.to_string(),
            iss: self.issuer.clone(),
            aud: audience.clone(),
            runtime_environment: self.runtime_environment,
            iat: now,
            nbf: now,
            exp: now + self.ttl as i64,
            jti: jti.clone(),
            delegated_context: delegated.clone(),
            scopes: scopes.clone(),
        };
        let mut header = Header::new(Algorithm::EdDSA);
        header.typ = Some(CONTACT_TOKEN_TYPE.to_string());
        header.kid = Some(self.key.kid().as_str().to_string());
        let token = jsonwebtoken::encode(&header, &claims, self.key.encoding_key())
            .map_err(|_| ContactSignError::Signing)?;
        Ok(ContactAccessToken {
            token: SecretString::from(token),
            expires_at: claims.exp,
            token_id: jti,
            scopes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContactTrustError {
    #[error("contact delegation trust bundle {path} cannot be read")]
    Unreadable { path: String },
    #[error("contact delegation trust bundle is malformed: {0}")]
    Malformed(String),
    #[error("unsupported contact delegation trust bundle version {0}")]
    UnsupportedVersion(u32),
    #[error("token_use must be {CONTACT_TOKEN_USE}")]
    WrongUse,
    #[error("{field} `{value}` is invalid")]
    Invalid { field: &'static str, value: String },
    #[error("bundle audience {bundle} does not match this service ({configured})")]
    AudienceMismatch {
        configured: ServiceId,
        bundle: ServiceId,
    },
    #[error("bundle runtime environment {bundle} does not match this service ({configured})")]
    RuntimeEnvironmentMismatch {
        configured: RuntimeEnvironment,
        bundle: RuntimeEnvironment,
    },
    #[error("a contact trust bundle must name 1..={MAX_ISSUERS} issuers")]
    IssuerCount,
    #[error("issuer {0} is listed more than once or is the audience itself")]
    BadIssuer(ServiceId),
    #[error("issuer {0} must have 1..={MAX_KEYS_PER_ISSUER} keys")]
    KeyCount(ServiceId),
    #[error("key id {0} appears more than once")]
    DuplicateKeyId(KeyId),
    #[error("key {0} must be an OKP / Ed25519 key")]
    UnsupportedKeyType(KeyId),
    #[error("{0}")]
    InvalidPublicKey(KeyError),
    #[error("issuer {0} must be allowed at least one scope, each once")]
    Scopes(ServiceId),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContactBundle {
    bundle_version: u32,
    token_use: String,
    audience: String,
    runtime_environment: String,
    trusted_issuers: Vec<RawContactIssuer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContactIssuer {
    issuer: String,
    keys: Vec<RawContactKey>,
    scopes: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContactKey {
    kid: String,
    kty: String,
    crv: String,
    x: String,
}

struct ContactTrustedKey {
    issuer: ServiceId,
    key: DecodingKey,
    x: String,
}

/// Receiver-authoritative contact delegation trust: which issuers' keys are accepted for
/// contact access tokens addressed to this service, and which scopes each may delegate.
pub struct ContactDelegationTrust {
    audience: ServiceId,
    runtime_environment: RuntimeEnvironment,
    keys: BTreeMap<KeyId, ContactTrustedKey>,
    scopes: BTreeMap<ServiceId, BTreeSet<ContactScope>>,
}

impl fmt::Debug for ContactDelegationTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactDelegationTrust")
            .field("audience", &self.audience)
            .field("runtime_environment", &self.runtime_environment)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl ContactDelegationTrust {
    pub fn from_json(
        json: &str,
        audience: &ServiceId,
        environment: RuntimeEnvironment,
    ) -> Result<Self, ContactTrustError> {
        let raw: RawContactBundle =
            serde_json::from_str(json).map_err(|e| ContactTrustError::Malformed(e.to_string()))?;
        if raw.bundle_version != CONTACT_TRUST_BUNDLE_VERSION {
            return Err(ContactTrustError::UnsupportedVersion(raw.bundle_version));
        }
        if raw.token_use != CONTACT_TOKEN_USE {
            return Err(ContactTrustError::WrongUse);
        }
        let bundle_audience =
            ServiceId::parse(&raw.audience).map_err(|_| ContactTrustError::Invalid {
                field: "audience",
                value: bounded(&raw.audience),
            })?;
        if &bundle_audience != audience {
            return Err(ContactTrustError::AudienceMismatch {
                configured: audience.clone(),
                bundle: bundle_audience,
            });
        }
        let bundle_environment =
            RuntimeEnvironment::parse(&raw.runtime_environment).map_err(|_| {
                ContactTrustError::Invalid {
                    field: "runtime_environment",
                    value: bounded(&raw.runtime_environment),
                }
            })?;
        if bundle_environment != environment {
            return Err(ContactTrustError::RuntimeEnvironmentMismatch {
                configured: environment,
                bundle: bundle_environment,
            });
        }
        if raw.trusted_issuers.is_empty() || raw.trusted_issuers.len() > MAX_ISSUERS {
            return Err(ContactTrustError::IssuerCount);
        }
        let mut keys = BTreeMap::new();
        let mut scopes = BTreeMap::new();
        for entry in raw.trusted_issuers {
            let issuer =
                ServiceId::parse(&entry.issuer).map_err(|_| ContactTrustError::Invalid {
                    field: "issuer",
                    value: bounded(&entry.issuer),
                })?;
            if &issuer == audience || scopes.contains_key(&issuer) {
                return Err(ContactTrustError::BadIssuer(issuer));
            }
            if entry.keys.is_empty() || entry.keys.len() > MAX_KEYS_PER_ISSUER {
                return Err(ContactTrustError::KeyCount(issuer));
            }
            for key in entry.keys {
                let kid = KeyId::parse(&key.kid).map_err(|_| ContactTrustError::Invalid {
                    field: "kid",
                    value: bounded(&key.kid),
                })?;
                if keys.contains_key(&kid) {
                    return Err(ContactTrustError::DuplicateKeyId(kid));
                }
                if key.kty != "OKP" || key.crv != "Ed25519" {
                    return Err(ContactTrustError::UnsupportedKeyType(kid));
                }
                let decoding = decoding_key(kid.as_str(), &key.x)
                    .map_err(ContactTrustError::InvalidPublicKey)?;
                keys.insert(
                    kid,
                    ContactTrustedKey {
                        issuer: issuer.clone(),
                        key: decoding,
                        x: key.x,
                    },
                );
            }
            let mut allowed = BTreeSet::new();
            for value in &entry.scopes {
                let scope = ContactScope::parse(value).ok_or(ContactTrustError::Invalid {
                    field: "scope",
                    value: bounded(value),
                })?;
                if !allowed.insert(scope) {
                    return Err(ContactTrustError::Scopes(issuer));
                }
            }
            if allowed.is_empty() {
                return Err(ContactTrustError::Scopes(issuer));
            }
            scopes.insert(issuer, allowed);
        }
        Ok(Self {
            audience: audience.clone(),
            runtime_environment: environment,
            keys,
            scopes,
        })
    }

    pub fn from_file(
        path: &Path,
        audience: &ServiceId,
        environment: RuntimeEnvironment,
    ) -> Result<Self, ContactTrustError> {
        let json = std::fs::read_to_string(path).map_err(|_| ContactTrustError::Unreadable {
            path: path.display().to_string(),
        })?;
        Self::from_json(&json, audience, environment)
    }

    pub fn audience(&self) -> &ServiceId {
        &self.audience
    }

    /// Public keys trusted for contact access tokens.
    pub fn public_keys(&self) -> impl Iterator<Item = &str> {
        self.keys.values().map(|k| k.x.as_str())
    }
}

/// Why a presented contact access token was refused (operator diagnostics only; the
/// customer always sees the same `CONTACT_DELEGATION_INVALID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactDelegationFailure {
    Missing,
    Duplicate,
    CustomerCredentialPresent,
    MalformedAuthorization,
    MalformedToken,
    UnsupportedAlgorithm,
    UnsupportedTokenType,
    ForbiddenHeaderParameter,
    UnknownKey,
    BadSignature,
    MalformedClaims,
    UnsupportedVersion,
    WrongUse,
    IssuerKeyMismatch,
    AudienceMismatch,
    RuntimeEnvironmentMismatch,
    InvalidTimeClaims,
    LifetimeTooLong,
    IssuedInFuture,
    NotYetValid,
    Expired,
    NoScopes,
    ScopeNotDelegable,
}

impl ContactDelegationFailure {
    pub fn as_str(&self) -> &'static str {
        use ContactDelegationFailure::*;
        match self {
            Missing => "missing_contact_token",
            Duplicate => "duplicate_contact_token",
            CustomerCredentialPresent => "customer_credential_present",
            MalformedAuthorization => "malformed_authorization",
            MalformedToken => "malformed_token",
            UnsupportedAlgorithm => "unsupported_algorithm",
            UnsupportedTokenType => "unsupported_token_type",
            ForbiddenHeaderParameter => "forbidden_header_parameter",
            UnknownKey => "unknown_key",
            BadSignature => "bad_signature",
            MalformedClaims => "malformed_claims",
            UnsupportedVersion => "unsupported_version",
            WrongUse => "wrong_use",
            IssuerKeyMismatch => "issuer_key_mismatch",
            AudienceMismatch => "audience_mismatch",
            RuntimeEnvironmentMismatch => "runtime_environment_mismatch",
            InvalidTimeClaims => "invalid_time_claims",
            LifetimeTooLong => "lifetime_too_long",
            IssuedInFuture => "issued_in_future",
            NotYetValid => "not_yet_valid",
            Expired => "expired",
            NoScopes => "no_scopes",
            ScopeNotDelegable => "scope_not_delegable",
        }
    }
}

/// A verified contact access delegation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactAccess {
    issuer: ServiceId,
    token_id: TokenId,
    key_id: KeyId,
    expires_at: i64,
    delegated: DelegatedPlatformContext,
    scopes: BTreeSet<ContactScope>,
}

impl ContactAccess {
    pub fn issuer(&self) -> &ServiceId {
        &self.issuer
    }

    pub fn token_id(&self) -> &TokenId {
        &self.token_id
    }

    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    pub fn delegated(&self) -> &DelegatedPlatformContext {
        &self.delegated
    }

    pub fn has_scope(&self, scope: ContactScope) -> bool {
        self.scopes.contains(&scope)
    }

    pub fn scopes(&self) -> impl Iterator<Item = ContactScope> + '_ {
        self.scopes.iter().copied()
    }
}

pub struct ContactDelegationVerifier {
    trust: ContactDelegationTrust,
    clock: SharedClock,
}

impl fmt::Debug for ContactDelegationVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactDelegationVerifier")
            .field("trust", &self.trust)
            .finish()
    }
}

impl ContactDelegationVerifier {
    pub fn new(trust: ContactDelegationTrust, clock: SharedClock) -> Self {
        Self { trust, clock }
    }

    pub fn trust(&self) -> &ContactDelegationTrust {
        &self.trust
    }

    /// Verifies the request headers: exactly one `X-CF-Contact-Authorization`, no customer
    /// `Authorization` header.
    pub fn verify_headers(
        &self,
        headers: &HeaderMap,
    ) -> Result<ContactAccess, ContactDelegationFailure> {
        if headers.contains_key(AUTHORIZATION) {
            return Err(ContactDelegationFailure::CustomerCredentialPresent);
        }
        let mut values = headers.get_all(CONTACT_AUTHORIZATION).iter();
        let value = values.next().ok_or(ContactDelegationFailure::Missing)?;
        if values.next().is_some() {
            return Err(ContactDelegationFailure::Duplicate);
        }
        let value = value
            .to_str()
            .map_err(|_| ContactDelegationFailure::MalformedAuthorization)?;
        self.verify(value)
    }

    /// Verifies one `Bearer <token>` value.
    pub fn verify(&self, authorization: &str) -> Result<ContactAccess, ContactDelegationFailure> {
        use ContactDelegationFailure as F;
        let token = bearer_token(authorization).ok_or(F::MalformedAuthorization)?;
        let (header_segment, _, _) = split_compact(token).ok_or(F::MalformedToken)?;
        let header =
            parse_protected_header_of(header_segment, CONTACT_TOKEN_TYPE).map_err(|p| match p {
                HeaderProblem::Malformed => F::MalformedToken,
                HeaderProblem::Algorithm => F::UnsupportedAlgorithm,
                HeaderProblem::TokenType => F::UnsupportedTokenType,
                HeaderProblem::ForbiddenParameter => F::ForbiddenHeaderParameter,
            })?;
        let trusted = self.trust.keys.get(&header.kid).ok_or(F::UnknownKey)?;
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.algorithms = vec![Algorithm::EdDSA];
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.leeway = 0;
        let decoded = jsonwebtoken::decode::<serde_json::Value>(token, &trusted.key, &validation)
            .map_err(|e| match e.kind() {
            ErrorKind::InvalidSignature => F::BadSignature,
            ErrorKind::InvalidAlgorithm => F::UnsupportedAlgorithm,
            _ => F::MalformedToken,
        })?;
        let claims: ContactDelegationClaims =
            serde_json::from_value(decoded.claims).map_err(|_| F::MalformedClaims)?;
        if claims.ver != CONTACT_CLAIMS_VERSION {
            return Err(F::UnsupportedVersion);
        }
        if claims.token_use != CONTACT_TOKEN_USE {
            return Err(F::WrongUse);
        }
        if claims.iss != trusted.issuer {
            return Err(F::IssuerKeyMismatch);
        }
        if claims.aud != self.trust.audience {
            return Err(F::AudienceMismatch);
        }
        if claims.runtime_environment != self.trust.runtime_environment {
            return Err(F::RuntimeEnvironmentMismatch);
        }
        let now = self.clock.now_unix();
        let skew = MAX_CLOCK_SKEW_SECONDS as i64;
        if claims.iat < 0 || claims.nbf < claims.iat || claims.exp <= claims.nbf {
            return Err(F::InvalidTimeClaims);
        }
        if claims.exp - claims.iat > MAX_TTL_SECONDS as i64 {
            return Err(F::LifetimeTooLong);
        }
        if claims.iat > now + skew {
            return Err(F::IssuedInFuture);
        }
        if now + skew < claims.nbf {
            return Err(F::NotYetValid);
        }
        if now - skew >= claims.exp {
            return Err(F::Expired);
        }
        if claims.scopes.is_empty() {
            return Err(F::NoScopes);
        }
        let allowed = self
            .trust
            .scopes
            .get(&claims.iss)
            .ok_or(F::IssuerKeyMismatch)?;
        let scopes: BTreeSet<ContactScope> = claims.scopes.iter().copied().collect();
        if scopes.len() != claims.scopes.len() || !scopes.is_subset(allowed) {
            return Err(F::ScopeNotDelegable);
        }
        Ok(ContactAccess {
            issuer: claims.iss,
            token_id: claims.jti,
            key_id: header.kid,
            expires_at: claims.exp,
            delegated: claims.delegated_context,
            scopes,
        })
    }
}

/// `Bearer <token>` header value for a minted token (sensitive: redacted by HTTP
/// libraries' debug output). Clients and tests only.
pub fn contact_authorization_value(token: &ContactAccessToken) -> HeaderValue {
    let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose_for_delivery()))
        .expect("compact JWS is a valid header value");
    value.set_sensitive(true);
    value
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        clock::ManualClock,
        delegated::ApplicationEnvironmentKind,
        ids::CorrelationId,
        signer::{OutboundServiceRequest, ServiceTokenSigner, TokenTtl},
    };

    const T0: i64 = 1_900_000_000;

    fn delegated() -> DelegatedPlatformContext {
        DelegatedPlatformContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            ApplicationEnvironmentKind::Production,
            uuid::Uuid::new_v4(),
        )
    }

    fn key(kid: &str) -> ServiceSigningKey {
        ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
    }

    fn bundle(keys: &[&ServiceSigningKey], scopes: &[&str]) -> String {
        serde_json::json!({
            "bundle_version": 1,
            "token_use": "CONTACT_VAULT_ACCESS",
            "audience": "cfmeet.privacy-relay",
            "runtime_environment": "staging",
            "trusted_issuers": [{
                "issuer": "cfmeet.calling",
                "keys": keys.iter().map(|k| k.public_jwk()).collect::<Vec<_>>(),
                "scopes": scopes,
            }],
        })
        .to_string()
    }

    struct Fixture {
        clock: ManualClock,
        signer: ContactDelegationSigner,
        verifier: ContactDelegationVerifier,
    }

    fn fixture() -> Fixture {
        let clock = ManualClock::new(T0);
        let shared: SharedClock = Arc::new(clock.clone());
        let k = key("contact-delegation-1");
        let trust = ContactDelegationTrust::from_json(
            &bundle(&[&k], &["privacy.contacts.read", "privacy.contacts.write"]),
            &ServiceId::privacy_relay(),
            RuntimeEnvironment::Staging,
        )
        .unwrap();
        Fixture {
            signer: ContactDelegationSigner::new(
                ServiceId::calling(),
                RuntimeEnvironment::Staging,
                k,
                DEFAULT_CONTACT_TTL_SECONDS,
                shared.clone(),
            )
            .unwrap(),
            verifier: ContactDelegationVerifier::new(trust, shared),
            clock,
        }
    }

    fn bearer(t: &ContactAccessToken) -> String {
        format!("Bearer {}", t.expose_for_delivery())
    }

    #[test]
    fn a_minted_token_verifies_with_its_scope_and_expires() {
        let f = fixture();
        let d = delegated();
        let t = f
            .signer
            .sign(
                &ServiceId::privacy_relay(),
                &d,
                &[ContactScope::Write, ContactScope::Write],
            )
            .unwrap();
        assert_eq!(t.scopes(), &[ContactScope::Write]);
        assert_eq!(t.expires_at(), T0 + 120);
        assert!(!format!("{t:?}").contains(t.expose_for_delivery()));
        let access = f.verifier.verify(&bearer(&t)).unwrap();
        assert_eq!(access.delegated(), &d);
        assert!(access.has_scope(ContactScope::Write));
        assert!(!access.has_scope(ContactScope::Read));
        f.clock.advance(120 + 30);
        assert_eq!(
            f.verifier.verify(&bearer(&t)),
            Err(ContactDelegationFailure::Expired)
        );
    }

    #[test]
    fn scopes_beyond_the_receivers_grant_are_refused() {
        let f = fixture();
        let t = f
            .signer
            .sign(
                &ServiceId::privacy_relay(),
                &delegated(),
                &[ContactScope::Verify],
            )
            .unwrap();
        assert_eq!(
            f.verifier.verify(&bearer(&t)),
            Err(ContactDelegationFailure::ScopeNotDelegable)
        );
        assert_eq!(
            f.signer
                .sign(&ServiceId::privacy_relay(), &delegated(), &[])
                .unwrap_err(),
            ContactSignError::NoScopes
        );
    }

    #[test]
    fn wrong_audience_environment_or_key_are_refused() {
        let f = fixture();
        let t = f
            .signer
            .sign(&ServiceId::telephony(), &delegated(), &[ContactScope::Read])
            .unwrap();
        assert_eq!(
            f.verifier.verify(&bearer(&t)),
            Err(ContactDelegationFailure::AudienceMismatch)
        );
        // Another environment's signer (same key id, different key).
        let other = ContactDelegationSigner::new(
            ServiceId::calling(),
            RuntimeEnvironment::Production,
            key("contact-delegation-1"),
            120,
            Arc::new(f.clock.clone()),
        )
        .unwrap();
        let t = other
            .sign(
                &ServiceId::privacy_relay(),
                &delegated(),
                &[ContactScope::Read],
            )
            .unwrap();
        assert_eq!(
            f.verifier.verify(&bearer(&t)),
            Err(ContactDelegationFailure::BadSignature)
        );
        let unknown = ContactDelegationSigner::new(
            ServiceId::calling(),
            RuntimeEnvironment::Staging,
            key("contact-delegation-9"),
            120,
            Arc::new(f.clock.clone()),
        )
        .unwrap();
        let t = unknown
            .sign(
                &ServiceId::privacy_relay(),
                &delegated(),
                &[ContactScope::Read],
            )
            .unwrap();
        assert_eq!(
            f.verifier.verify(&bearer(&t)),
            Err(ContactDelegationFailure::UnknownKey)
        );
    }

    #[test]
    fn a_service_token_is_never_a_contact_token_and_vice_versa() {
        let f = fixture();
        // A service token signed with the SAME key material is still refused by type.
        let service_key = key("contact-delegation-1");
        let jwk = service_key.public_jwk();
        let shared: SharedClock = Arc::new(f.clock.clone());
        let service_signer = ServiceTokenSigner::new(
            ServiceId::calling(),
            RuntimeEnvironment::Staging,
            service_key,
            TokenTtl::default(),
            shared.clone(),
        );
        let trust = ContactDelegationTrust::from_json(
            &serde_json::json!({
                "bundle_version": 1, "token_use": "CONTACT_VAULT_ACCESS",
                "audience": "cfmeet.privacy-relay", "runtime_environment": "staging",
                "trusted_issuers": [{ "issuer": "cfmeet.calling", "keys": [jwk], "scopes": ["privacy.contacts.read"] }],
            })
            .to_string(),
            &ServiceId::privacy_relay(),
            RuntimeEnvironment::Staging,
        )
        .unwrap();
        let verifier = ContactDelegationVerifier::new(trust, shared);
        let signed = service_signer
            .sign(
                &OutboundServiceRequest::continuing(
                    ServiceId::privacy_relay(),
                    &CorrelationId::generate(),
                )
                .with_delegated_context(delegated()),
            )
            .unwrap();
        let value = format!("Bearer {}", signed.token().expose_for_transport());
        assert_eq!(
            verifier.verify(&value),
            Err(ContactDelegationFailure::UnsupportedTokenType)
        );

        // A contact token presented to the service-token verifier.
        let t = f
            .signer
            .sign(
                &ServiceId::privacy_relay(),
                &delegated(),
                &[ContactScope::Read],
            )
            .unwrap();
        let (h, _, _) = split_compact(t.expose_for_delivery()).unwrap();
        assert_eq!(
            crate::claims::parse_protected_header(h).unwrap_err(),
            HeaderProblem::TokenType
        );
    }

    #[test]
    fn header_channel_rules() {
        let f = fixture();
        let t = f
            .signer
            .sign(
                &ServiceId::privacy_relay(),
                &delegated(),
                &[ContactScope::Read],
            )
            .unwrap();
        let mut h = HeaderMap::new();
        assert_eq!(
            f.verifier.verify_headers(&h),
            Err(ContactDelegationFailure::Missing)
        );
        h.insert(CONTACT_AUTHORIZATION, contact_authorization_value(&t));
        assert!(f.verifier.verify_headers(&h).is_ok());
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer cfmk_live_x_y"),
        );
        assert_eq!(
            f.verifier.verify_headers(&h),
            Err(ContactDelegationFailure::CustomerCredentialPresent)
        );
        h.remove(AUTHORIZATION);
        h.append(CONTACT_AUTHORIZATION, contact_authorization_value(&t));
        assert_eq!(
            f.verifier.verify_headers(&h),
            Err(ContactDelegationFailure::Duplicate)
        );
    }

    #[test]
    fn bundles_are_validated() {
        let k = key("k-1");
        let ok = bundle(&[&k], &["privacy.contacts.read"]);
        let env = RuntimeEnvironment::Staging;
        let pr = ServiceId::privacy_relay();
        assert!(ContactDelegationTrust::from_json(&ok, &pr, env).is_ok());
        assert!(matches!(
            ContactDelegationTrust::from_json(&ok, &ServiceId::calling(), env),
            Err(ContactTrustError::AudienceMismatch { .. })
        ));
        assert!(matches!(
            ContactDelegationTrust::from_json(&ok, &pr, RuntimeEnvironment::Production),
            Err(ContactTrustError::RuntimeEnvironmentMismatch { .. })
        ));
        for bad in [
            ok.replace("CONTACT_VAULT_ACCESS", "SERVICE"),
            ok.replace("privacy.contacts.read", "privacy.contacts.*"),
            bundle(&[&k], &[]),
            bundle(&[], &["privacy.contacts.read"]),
        ] {
            assert!(
                ContactDelegationTrust::from_json(&bad, &pr, env).is_err(),
                "{bad}"
            );
        }
    }
}
