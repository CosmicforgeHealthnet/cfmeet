//! Number routing grants (Part IV Workstream K): short-lived, signed statements by
//! Numbering that a number may be used for ONE routing purpose at ONE routing revision.
//!
//! * `INBOUND_VOICE` / `INBOUND_SMS` — the resolution of a received call / message to the
//!   owner scope and routing target of the number's ACTIVE assignment (requested by the
//!   carrier of the traffic, addressed to the service that acts on it);
//! * `CALLER_IDENTITY` — a customer scope may present its own number as the caller
//!   identity of one outbound call (requested by Calling, addressed to Telephony);
//! * `SMS_SENDER` — a customer scope may send one message from its own number (requested
//!   and consumed by the SMS Gateway).
//!
//! A grant is cryptographically SEPARATE from every other credential:
//!
//! * its own JWS type `cf-number-grant+jwt` (a service token or a contact access token is
//!   refused here, and a grant is refused by their verifiers);
//! * its own Ed25519 signing key (never the service-auth key; Numbering refuses to start
//!   with one key for both, and a grant trust bundle may not list a service-token key);
//! * its own receiver-authoritative trust bundle (contract `cfmeet.numbering.routing-grant`):
//!   which issuers' keys are accepted for which grant kinds;
//! * a closed claim set — `use` = `NUMBER_ROUTING_GRANT`, one audience, the runtime
//!   environment, at most 300 s of lifetime, the number, the assignment and the number's
//!   ROUTING REVISION it was issued at. A grant carries no telephone number: the receiver
//!   that needs one gets it from Numbering's verification.
//!
//! A signature proves only that Numbering issued the grant. Revocation is immediate
//! because the AUTHORITATIVE check is Numbering's verification against current state:
//! the assignment still ACTIVE, the routing revision unchanged, the capability usable and
//! (for outbound grants) the single operation not already performed.

use std::{collections::BTreeMap, collections::BTreeSet, fmt, path::Path};

use jsonwebtoken::{errors::ErrorKind, Algorithm, DecodingKey, Header, Validation};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    claims::{
        parse_protected_header_of, split_compact, HeaderProblem, MAX_CLOCK_SKEW_SECONDS,
        MAX_TTL_SECONDS, MIN_TTL_SECONDS,
    },
    clock::SharedClock,
    identity::{bounded, RuntimeEnvironment, ServiceId},
    ids::{KeyId, TokenId},
    keys::{decoding_key, KeyError, ServiceSigningKey},
};

/// JWS `typ` of a number routing grant.
pub const NUMBER_GRANT_TOKEN_TYPE: &str = "cf-number-grant+jwt";
/// The only accepted `use` claim.
pub const NUMBER_GRANT_USE: &str = "NUMBER_ROUTING_GRANT";
/// Claims contract major version.
pub const NUMBER_GRANT_CLAIMS_VERSION: u8 = 1;
/// Default lifetime.
pub const DEFAULT_NUMBER_GRANT_TTL_SECONDS: u64 = 60;
/// Trust bundle format version.
pub const NUMBER_GRANT_TRUST_BUNDLE_VERSION: u32 = 1;
const MAX_KEYS_PER_ISSUER: usize = 4;
const MAX_ISSUERS: usize = 4;
const MAX_OPERATION_REF: usize = 128;

/// What a grant authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NumberGrantKind {
    InboundVoice,
    InboundSms,
    CallerIdentity,
    SmsSender,
}

impl NumberGrantKind {
    pub const ALL: [NumberGrantKind; 4] = [
        NumberGrantKind::InboundVoice,
        NumberGrantKind::InboundSms,
        NumberGrantKind::CallerIdentity,
        NumberGrantKind::SmsSender,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            NumberGrantKind::InboundVoice => "INBOUND_VOICE",
            NumberGrantKind::InboundSms => "INBOUND_SMS",
            NumberGrantKind::CallerIdentity => "CALLER_IDENTITY",
            NumberGrantKind::SmsSender => "SMS_SENDER",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == value)
    }

    /// Outbound grants bind one operation (`operation_ref`) and are consumed once.
    pub fn is_outbound(&self) -> bool {
        matches!(
            self,
            NumberGrantKind::CallerIdentity | NumberGrantKind::SmsSender
        )
    }
}

impl fmt::Display for NumberGrantKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The customer scope a grant is for (from Numbering's records, never a request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantScope {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

/// An opaque CF Meet routing target (inbound grants).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantTarget {
    pub kind: String,
    #[serde(rename = "ref")]
    pub target_ref: Uuid,
}

/// Closed claim set (contract `cfmeet.numbering.routing-grant`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberGrantClaims {
    ver: u8,
    #[serde(rename = "use")]
    token_use: String,
    grant_kind: NumberGrantKind,
    iss: ServiceId,
    aud: ServiceId,
    runtime_environment: RuntimeEnvironment,
    iat: i64,
    nbf: i64,
    exp: i64,
    jti: TokenId,
    number_resource_id: Uuid,
    assignment_id: Uuid,
    routing_revision: i64,
    scope: GrantScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    principal_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<GrantTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_ref: Option<String>,
}

impl NumberGrantClaims {
    /// Parses claims JSON (contract conformance only; parsing establishes no trust).
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

fn valid_operation_ref(value: &str) -> bool {
    (8..=MAX_OPERATION_REF).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn valid_target_kind(value: &str) -> bool {
    (2..=40).contains(&value.len())
        && value.as_bytes()[0].is_ascii_uppercase()
        && value.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        && value != "NONE"
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NumberGrantSignError {
    #[error("grant lifetime must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS} seconds")]
    InvalidTtl,
    #[error("a grant cannot be addressed to its issuer")]
    SelfAudience,
    #[error("an inbound grant names its routing target and no operation; an outbound grant names one operation and no target")]
    Shape,
    #[error("token signing failed")]
    Signing,
}

/// What a grant states (filled from Numbering's own records).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberGrantRequest {
    pub kind: NumberGrantKind,
    pub audience: ServiceId,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub scope: GrantScope,
    pub principal_id: Option<Uuid>,
    pub target: Option<GrantTarget>,
    pub operation_ref: Option<String>,
}

/// A minted grant. `Debug` is redacted; there is no `Display` / `Serialize`.
pub struct NumberGrant {
    token: SecretString,
    token_id: TokenId,
    issued_at: i64,
    expires_at: i64,
}

impl NumberGrant {
    /// The compact JWS, to hand to the requesting service exactly once.
    pub fn expose_for_delivery(&self) -> &str {
        self.token.expose_secret()
    }

    pub fn token_id(&self) -> &TokenId {
        &self.token_id
    }

    pub fn issued_at(&self) -> i64 {
        self.issued_at
    }

    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }
}

impl fmt::Debug for NumberGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberGrant")
            .field("token", &"[REDACTED]")
            .field("token_id", &self.token_id)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Mints grants with the issuer's DEDICATED grant key.
pub struct NumberGrantSigner {
    issuer: ServiceId,
    runtime_environment: RuntimeEnvironment,
    key: ServiceSigningKey,
    ttl: u64,
    clock: SharedClock,
}

impl fmt::Debug for NumberGrantSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberGrantSigner")
            .field("issuer", &self.issuer)
            .field("kid", self.key.kid())
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl NumberGrantSigner {
    pub fn new(
        issuer: ServiceId,
        runtime_environment: RuntimeEnvironment,
        key: ServiceSigningKey,
        ttl_seconds: u64,
        clock: SharedClock,
    ) -> Result<Self, NumberGrantSignError> {
        if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl_seconds) {
            return Err(NumberGrantSignError::InvalidTtl);
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

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime_environment
    }

    pub fn key_id(&self) -> &KeyId {
        self.key.kid()
    }

    /// Public key of the grant key (published in receivers' grant trust bundles).
    pub fn public_jwk(&self) -> crate::keys::PublicJwk {
        self.key.public_jwk()
    }

    pub fn ttl_seconds(&self) -> u64 {
        self.ttl
    }

    pub fn sign(&self, request: &NumberGrantRequest) -> Result<NumberGrant, NumberGrantSignError> {
        if request.audience == self.issuer {
            return Err(NumberGrantSignError::SelfAudience);
        }
        let shape_ok = if request.kind.is_outbound() {
            request.target.is_none()
                && request
                    .operation_ref
                    .as_deref()
                    .is_some_and(valid_operation_ref)
        } else {
            request.operation_ref.is_none()
                && request
                    .target
                    .as_ref()
                    .is_some_and(|t| valid_target_kind(&t.kind))
        };
        if !shape_ok || request.routing_revision < 0 {
            return Err(NumberGrantSignError::Shape);
        }
        let now = self.clock.now_unix();
        let jti = TokenId::generate();
        let claims = NumberGrantClaims {
            ver: NUMBER_GRANT_CLAIMS_VERSION,
            token_use: NUMBER_GRANT_USE.to_string(),
            grant_kind: request.kind,
            iss: self.issuer.clone(),
            aud: request.audience.clone(),
            runtime_environment: self.runtime_environment,
            iat: now,
            nbf: now,
            exp: now + self.ttl as i64,
            jti: jti.clone(),
            number_resource_id: request.number_resource_id,
            assignment_id: request.assignment_id,
            routing_revision: request.routing_revision,
            scope: request.scope,
            principal_id: request.principal_id,
            target: request.target.clone(),
            operation_ref: request.operation_ref.clone(),
        };
        let mut header = Header::new(Algorithm::EdDSA);
        header.typ = Some(NUMBER_GRANT_TOKEN_TYPE.to_string());
        header.kid = Some(self.key.kid().as_str().to_string());
        let token = crate::jwt::encode(&header, &claims, self.key.encoding_key())
            .map_err(|_| NumberGrantSignError::Signing)?;
        Ok(NumberGrant {
            token: SecretString::from(token),
            token_id: jti,
            issued_at: now,
            expires_at: claims.exp,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NumberGrantTrustError {
    #[error("number grant trust bundle {path} cannot be read")]
    Unreadable { path: String },
    #[error("number grant trust bundle is malformed: {0}")]
    Malformed(String),
    #[error("unsupported number grant trust bundle version {0}")]
    UnsupportedVersion(u32),
    #[error("token_use must be {NUMBER_GRANT_USE}")]
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
    #[error("a number grant trust bundle must name 1..={MAX_ISSUERS} issuers")]
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
    #[error("issuer {0} must be trusted for at least one grant kind, each once")]
    Kinds(ServiceId),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBundle {
    bundle_version: u32,
    token_use: String,
    audience: String,
    runtime_environment: String,
    trusted_issuers: Vec<RawIssuer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIssuer {
    issuer: String,
    keys: Vec<RawKey>,
    grant_kinds: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    kid: String,
    kty: String,
    crv: String,
    x: String,
}

struct TrustedGrantKey {
    issuer: ServiceId,
    key: DecodingKey,
    x: String,
}

/// Receiver-authoritative grant trust: which issuers' keys are accepted for grants
/// addressed to this service, and for which grant kinds.
pub struct NumberGrantTrust {
    audience: ServiceId,
    runtime_environment: RuntimeEnvironment,
    keys: BTreeMap<KeyId, TrustedGrantKey>,
    kinds: BTreeMap<ServiceId, BTreeSet<NumberGrantKind>>,
}

impl fmt::Debug for NumberGrantTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberGrantTrust")
            .field("audience", &self.audience)
            .field("runtime_environment", &self.runtime_environment)
            .field("key_ids", &self.keys.keys().collect::<Vec<_>>())
            .field("kinds", &self.kinds)
            .finish()
    }
}

impl NumberGrantTrust {
    pub fn from_json(
        json: &str,
        audience: &ServiceId,
        environment: RuntimeEnvironment,
    ) -> Result<Self, NumberGrantTrustError> {
        use NumberGrantTrustError as E;
        let raw: RawBundle = serde_json::from_str(json).map_err(|e| E::Malformed(e.to_string()))?;
        if raw.bundle_version != NUMBER_GRANT_TRUST_BUNDLE_VERSION {
            return Err(E::UnsupportedVersion(raw.bundle_version));
        }
        if raw.token_use != NUMBER_GRANT_USE {
            return Err(E::WrongUse);
        }
        let bundle_audience = ServiceId::parse(&raw.audience).map_err(|_| E::Invalid {
            field: "audience",
            value: bounded(&raw.audience),
        })?;
        if &bundle_audience != audience {
            return Err(E::AudienceMismatch {
                configured: audience.clone(),
                bundle: bundle_audience,
            });
        }
        let bundle_environment =
            RuntimeEnvironment::parse(&raw.runtime_environment).map_err(|_| E::Invalid {
                field: "runtime_environment",
                value: bounded(&raw.runtime_environment),
            })?;
        if bundle_environment != environment {
            return Err(E::RuntimeEnvironmentMismatch {
                configured: environment,
                bundle: bundle_environment,
            });
        }
        if raw.trusted_issuers.is_empty() || raw.trusted_issuers.len() > MAX_ISSUERS {
            return Err(E::IssuerCount);
        }
        let mut keys = BTreeMap::new();
        let mut kinds = BTreeMap::new();
        for entry in raw.trusted_issuers {
            let issuer = ServiceId::parse(&entry.issuer).map_err(|_| E::Invalid {
                field: "issuer",
                value: bounded(&entry.issuer),
            })?;
            if &issuer == audience || kinds.contains_key(&issuer) {
                return Err(E::BadIssuer(issuer));
            }
            if entry.keys.is_empty() || entry.keys.len() > MAX_KEYS_PER_ISSUER {
                return Err(E::KeyCount(issuer));
            }
            for key in entry.keys {
                let kid = KeyId::parse(&key.kid).map_err(|_| E::Invalid {
                    field: "kid",
                    value: bounded(&key.kid),
                })?;
                if keys.contains_key(&kid) {
                    return Err(E::DuplicateKeyId(kid));
                }
                if key.kty != "OKP" || key.crv != "Ed25519" {
                    return Err(E::UnsupportedKeyType(kid));
                }
                let decoding = decoding_key(kid.as_str(), &key.x).map_err(E::InvalidPublicKey)?;
                keys.insert(
                    kid,
                    TrustedGrantKey {
                        issuer: issuer.clone(),
                        key: decoding,
                        x: key.x,
                    },
                );
            }
            let mut allowed = BTreeSet::new();
            for value in &entry.grant_kinds {
                let kind = NumberGrantKind::parse(value).ok_or(E::Invalid {
                    field: "grant_kind",
                    value: bounded(value),
                })?;
                if !allowed.insert(kind) {
                    return Err(E::Kinds(issuer));
                }
            }
            if allowed.is_empty() {
                return Err(E::Kinds(issuer));
            }
            kinds.insert(issuer, allowed);
        }
        Ok(Self {
            audience: audience.clone(),
            runtime_environment: environment,
            keys,
            kinds,
        })
    }

    pub fn from_file(
        path: &Path,
        audience: &ServiceId,
        environment: RuntimeEnvironment,
    ) -> Result<Self, NumberGrantTrustError> {
        let json =
            std::fs::read_to_string(path).map_err(|_| NumberGrantTrustError::Unreadable {
                path: path.display().to_string(),
            })?;
        Self::from_json(&json, audience, environment)
    }

    /// The issuer's own view: its grant keys, for verifying grants it issued to ANY
    /// audience (Numbering's authoritative verification; the audience is then compared
    /// with the authenticated caller).
    pub fn issuer_keys(
        issuer: &ServiceId,
        environment: RuntimeEnvironment,
        keys: &[crate::keys::PublicJwk],
    ) -> Result<Self, NumberGrantTrustError> {
        use NumberGrantTrustError as E;
        if keys.is_empty() || keys.len() > MAX_KEYS_PER_ISSUER {
            return Err(E::KeyCount(issuer.clone()));
        }
        let mut map = BTreeMap::new();
        for key in keys {
            if map.contains_key(&key.kid) {
                return Err(E::DuplicateKeyId(key.kid.clone()));
            }
            let decoding = decoding_key(key.kid.as_str(), &key.x).map_err(E::InvalidPublicKey)?;
            map.insert(
                key.kid.clone(),
                TrustedGrantKey {
                    issuer: issuer.clone(),
                    key: decoding,
                    x: key.x.clone(),
                },
            );
        }
        Ok(Self {
            audience: issuer.clone(),
            runtime_environment: environment,
            keys: map,
            kinds: BTreeMap::from([(issuer.clone(), NumberGrantKind::ALL.into_iter().collect())]),
        })
    }

    pub fn audience(&self) -> &ServiceId {
        &self.audience
    }

    /// Public keys trusted for grants (to refuse overlap with service-token keys).
    pub fn public_keys(&self) -> impl Iterator<Item = &str> {
        self.keys.values().map(|k| k.x.as_str())
    }
}

/// Why a presented grant was refused (diagnostics; callers see one stable code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberGrantFailure {
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
    KindNotTrusted,
    MalformedShape,
}

impl NumberGrantFailure {
    pub fn as_str(&self) -> &'static str {
        use NumberGrantFailure::*;
        match self {
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
            KindNotTrusted => "kind_not_trusted",
            MalformedShape => "malformed_shape",
        }
    }
}

/// A grant whose signature, type, audience, environment and lifetime were verified. It
/// says nothing about CURRENT state: Numbering's verification decides that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedNumberGrant {
    pub kind: NumberGrantKind,
    pub issuer: ServiceId,
    pub audience: ServiceId,
    pub token_id: TokenId,
    pub key_id: KeyId,
    pub issued_at: i64,
    pub expires_at: i64,
    pub number_resource_id: Uuid,
    pub assignment_id: Uuid,
    pub routing_revision: i64,
    pub scope: GrantScope,
    pub principal_id: Option<Uuid>,
    pub target: Option<GrantTarget>,
    pub operation_ref: Option<String>,
}

pub struct NumberGrantVerifier {
    trust: NumberGrantTrust,
    clock: SharedClock,
}

impl fmt::Debug for NumberGrantVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberGrantVerifier")
            .field("trust", &self.trust)
            .finish()
    }
}

impl NumberGrantVerifier {
    pub fn new(trust: NumberGrantTrust, clock: SharedClock) -> Self {
        Self { trust, clock }
    }

    pub fn trust(&self) -> &NumberGrantTrust {
        &self.trust
    }

    /// Verifies a grant addressed to this receiver.
    pub fn verify(&self, token: &str) -> Result<VerifiedNumberGrant, NumberGrantFailure> {
        let grant = self.verify_any_audience(token)?;
        if grant.audience != self.trust.audience {
            return Err(NumberGrantFailure::AudienceMismatch);
        }
        Ok(grant)
    }

    /// Verifies everything but the audience (the ISSUER's verification of a grant it
    /// issued; the caller compares the audience with the authenticated presenter).
    pub fn verify_any_audience(
        &self,
        token: &str,
    ) -> Result<VerifiedNumberGrant, NumberGrantFailure> {
        use NumberGrantFailure as F;
        let (header_segment, _, _) = split_compact(token).ok_or(F::MalformedToken)?;
        let header = parse_protected_header_of(header_segment, NUMBER_GRANT_TOKEN_TYPE).map_err(
            |p| match p {
                HeaderProblem::Malformed => F::MalformedToken,
                HeaderProblem::Algorithm => F::UnsupportedAlgorithm,
                HeaderProblem::TokenType => F::UnsupportedTokenType,
                HeaderProblem::ForbiddenParameter => F::ForbiddenHeaderParameter,
            },
        )?;
        let trusted = self.trust.keys.get(&header.kid).ok_or(F::UnknownKey)?;
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.algorithms = vec![Algorithm::EdDSA];
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.leeway = 0;
        let decoded = crate::jwt::decode::<serde_json::Value>(token, &trusted.key, &validation)
            .map_err(|e| match e.kind() {
                ErrorKind::InvalidSignature => F::BadSignature,
                ErrorKind::InvalidAlgorithm => F::UnsupportedAlgorithm,
                _ => F::MalformedToken,
            })?;
        let claims: NumberGrantClaims =
            serde_json::from_value(decoded.claims).map_err(|_| F::MalformedClaims)?;
        if claims.ver != NUMBER_GRANT_CLAIMS_VERSION {
            return Err(F::UnsupportedVersion);
        }
        if claims.token_use != NUMBER_GRANT_USE {
            return Err(F::WrongUse);
        }
        if claims.iss != trusted.issuer {
            return Err(F::IssuerKeyMismatch);
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
        let allowed = self
            .trust
            .kinds
            .get(&claims.iss)
            .ok_or(F::IssuerKeyMismatch)?;
        if !allowed.contains(&claims.grant_kind) {
            return Err(F::KindNotTrusted);
        }
        let shape_ok = if claims.grant_kind.is_outbound() {
            claims.target.is_none()
                && claims
                    .operation_ref
                    .as_deref()
                    .is_some_and(valid_operation_ref)
        } else {
            claims.operation_ref.is_none()
                && claims
                    .target
                    .as_ref()
                    .is_some_and(|t| valid_target_kind(&t.kind))
        };
        if !shape_ok || claims.routing_revision < 0 {
            return Err(F::MalformedShape);
        }
        Ok(VerifiedNumberGrant {
            kind: claims.grant_kind,
            issuer: claims.iss,
            audience: claims.aud,
            token_id: claims.jti,
            key_id: header.kid,
            issued_at: claims.iat,
            expires_at: claims.exp,
            number_resource_id: claims.number_resource_id,
            assignment_id: claims.assignment_id,
            routing_revision: claims.routing_revision,
            scope: claims.scope,
            principal_id: claims.principal_id,
            target: claims.target,
            operation_ref: claims.operation_ref,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        clock::ManualClock,
        contact_delegation::{ContactDelegationSigner, ContactScope},
        delegated::{ApplicationEnvironmentKind, DelegatedPlatformContext},
        signer::{OutboundServiceRequest, ServiceTokenSigner, TokenTtl},
    };

    const T0: i64 = 1_900_000_000;

    fn key(kid: &str) -> ServiceSigningKey {
        ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
    }

    fn bundle(keys: &[&ServiceSigningKey], kinds: &[&str]) -> String {
        serde_json::json!({
            "bundle_version": 1,
            "token_use": "NUMBER_ROUTING_GRANT",
            "audience": "cfmeet.telephony",
            "runtime_environment": "staging",
            "trusted_issuers": [{
                "issuer": "cfmeet.numbering",
                "keys": keys.iter().map(|k| k.public_jwk()).collect::<Vec<_>>(),
                "grant_kinds": kinds,
            }],
        })
        .to_string()
    }

    struct Fixture {
        clock: ManualClock,
        signer: NumberGrantSigner,
        verifier: NumberGrantVerifier,
    }

    fn fixture() -> Fixture {
        let clock = ManualClock::new(T0);
        let shared: SharedClock = Arc::new(clock.clone());
        let k = key("number-grant-1");
        let trust = NumberGrantTrust::from_json(
            &bundle(&[&k], &["CALLER_IDENTITY", "INBOUND_VOICE"]),
            &ServiceId::telephony(),
            RuntimeEnvironment::Staging,
        )
        .unwrap();
        Fixture {
            signer: NumberGrantSigner::new(
                ServiceId::numbering(),
                RuntimeEnvironment::Staging,
                k,
                DEFAULT_NUMBER_GRANT_TTL_SECONDS,
                shared.clone(),
            )
            .unwrap(),
            verifier: NumberGrantVerifier::new(trust, shared),
            clock,
        }
    }

    fn scope() -> GrantScope {
        GrantScope {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
        }
    }

    fn caller_identity() -> NumberGrantRequest {
        NumberGrantRequest {
            kind: NumberGrantKind::CallerIdentity,
            audience: ServiceId::telephony(),
            number_resource_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            routing_revision: 3,
            scope: scope(),
            principal_id: Some(Uuid::new_v4()),
            target: None,
            operation_ref: Some("call-0123456789".into()),
        }
    }

    #[test]
    fn a_grant_round_trips_and_carries_no_number_or_secret() {
        let f = fixture();
        let request = caller_identity();
        let grant = f.signer.sign(&request).unwrap();
        assert!(!format!("{grant:?}").contains(grant.expose_for_delivery()));
        let v = f.verifier.verify(grant.expose_for_delivery()).unwrap();
        assert_eq!(
            (
                v.kind,
                v.number_resource_id,
                v.routing_revision,
                v.operation_ref.as_deref()
            ),
            (
                NumberGrantKind::CallerIdentity,
                request.number_resource_id,
                3,
                Some("call-0123456789")
            )
        );
        assert_eq!(v.expires_at - v.issued_at, 60);
        // The payload names CF Meet ids only: no telephone number field exists.
        let payload = grant.expose_for_delivery().split('.').nth(1).unwrap();
        let json = String::from_utf8(
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, payload)
                .unwrap(),
        )
        .unwrap();
        assert!(!json.contains("e164") && !json.contains('+'));
        assert!(NumberGrantClaims::from_json(&json).is_ok());
    }

    #[test]
    fn grants_are_short_lived_and_audience_environment_and_kind_bound() {
        let f = fixture();
        let grant = f.signer.sign(&caller_identity()).unwrap();
        f.clock.advance(60 + MAX_CLOCK_SKEW_SECONDS as i64);
        assert_eq!(
            f.verifier.verify(grant.expose_for_delivery()).unwrap_err(),
            NumberGrantFailure::Expired
        );
        let f = fixture();
        let mut other = caller_identity();
        other.audience = ServiceId::calling();
        let grant = f.signer.sign(&other).unwrap();
        assert_eq!(
            f.verifier.verify(grant.expose_for_delivery()).unwrap_err(),
            NumberGrantFailure::AudienceMismatch
        );
        // The issuer's own verification accepts any audience (it compares it with the
        // presenter itself).
        assert!(f
            .verifier
            .verify_any_audience(grant.expose_for_delivery())
            .is_ok());
        let mut sms = caller_identity();
        sms.kind = NumberGrantKind::SmsSender;
        let grant = f.signer.sign(&sms).unwrap();
        assert_eq!(
            f.verifier.verify(grant.expose_for_delivery()).unwrap_err(),
            NumberGrantFailure::KindNotTrusted
        );
        assert!(NumberGrantSigner::new(
            ServiceId::numbering(),
            RuntimeEnvironment::Staging,
            key("number-grant-2"),
            MAX_TTL_SECONDS + 1,
            Arc::new(ManualClock::new(T0)),
        )
        .is_err());
        // Shape: an outbound grant names one operation; an inbound grant a target.
        let mut bad = caller_identity();
        bad.operation_ref = None;
        assert_eq!(
            f.signer.sign(&bad).unwrap_err(),
            NumberGrantSignError::Shape
        );
        let mut inbound = caller_identity();
        inbound.kind = NumberGrantKind::InboundVoice;
        assert_eq!(
            f.signer.sign(&inbound).unwrap_err(),
            NumberGrantSignError::Shape
        );
        inbound.operation_ref = None;
        inbound.target = Some(GrantTarget {
            kind: "ENTERPRISE_QUEUE".into(),
            target_ref: Uuid::new_v4(),
        });
        let grant = f.signer.sign(&inbound).unwrap();
        assert!(f.verifier.verify(grant.expose_for_delivery()).is_ok());
    }

    #[test]
    fn other_credentials_and_foreign_keys_are_never_grants() {
        let f = fixture();
        let shared: SharedClock = Arc::new(f.clock.clone());
        // A service token signed with the very same key is still not a grant (typ).
        let k = key("number-grant-1");
        let service = ServiceTokenSigner::new(
            ServiceId::numbering(),
            RuntimeEnvironment::Staging,
            k,
            TokenTtl::default(),
            shared.clone(),
        );
        let signed = service
            .sign(&OutboundServiceRequest::new_operation(
                ServiceId::telephony(),
            ))
            .unwrap();
        let bearer = signed
            .headers()
            .get(crate::headers::SERVICE_AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap()
            .trim_start_matches("Bearer ")
            .to_string();
        assert_eq!(
            f.verifier.verify(&bearer).unwrap_err(),
            NumberGrantFailure::UnsupportedTokenType
        );
        // A contact access token is not a grant either.
        let contact = ContactDelegationSigner::new(
            ServiceId::calling(),
            RuntimeEnvironment::Staging,
            key("number-grant-1"),
            120,
            shared.clone(),
        )
        .unwrap()
        .sign(
            &ServiceId::telephony(),
            &DelegatedPlatformContext::new(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                ApplicationEnvironmentKind::Production,
                Uuid::new_v4(),
            ),
            &[ContactScope::Read],
        )
        .unwrap();
        assert_eq!(
            f.verifier
                .verify(contact.expose_for_delivery())
                .unwrap_err(),
            NumberGrantFailure::UnsupportedTokenType
        );
        // An untrusted key with a trusted kid name is a bad signature.
        let forged = NumberGrantSigner::new(
            ServiceId::numbering(),
            RuntimeEnvironment::Staging,
            key("number-grant-1"),
            60,
            shared,
        )
        .unwrap()
        .sign(&caller_identity())
        .unwrap();
        assert_eq!(
            f.verifier.verify(forged.expose_for_delivery()).unwrap_err(),
            NumberGrantFailure::BadSignature
        );
        // Tampering with the payload breaks the signature.
        let good = f.signer.sign(&caller_identity()).unwrap();
        let parts: Vec<&str> = good.expose_for_delivery().split('.').collect();
        let other = f.signer.sign(&caller_identity()).unwrap();
        let other_payload = other.expose_for_delivery().split('.').nth(1).unwrap();
        let spliced = format!("{}.{}.{}", parts[0], other_payload, parts[2]);
        assert_eq!(
            f.verifier.verify(&spliced).unwrap_err(),
            NumberGrantFailure::BadSignature
        );
    }

    #[test]
    fn trust_bundles_are_strict() {
        let k = key("number-grant-1");
        let ok = |json: &str| {
            NumberGrantTrust::from_json(json, &ServiceId::telephony(), RuntimeEnvironment::Staging)
        };
        assert!(ok(&bundle(&[&k], &["CALLER_IDENTITY"])).is_ok());
        assert!(ok(&bundle(&[&k], &[])).is_err());
        assert!(ok(&bundle(&[&k], &["CALLER_IDENTITY", "CALLER_IDENTITY"])).is_err());
        assert!(ok(&bundle(&[&k], &["*"])).is_err());
        assert!(ok(&bundle(&[], &["CALLER_IDENTITY"])).is_err());
        assert!(ok(&bundle(&[&k], &["CALLER_IDENTITY"])
            .replace("NUMBER_ROUTING_GRANT", "CONTACT_VAULT_ACCESS"))
        .is_err());
        assert!(NumberGrantTrust::from_json(
            &bundle(&[&k], &["CALLER_IDENTITY"]),
            &ServiceId::calling(),
            RuntimeEnvironment::Staging
        )
        .is_err());
        assert!(NumberGrantTrust::from_json(
            &bundle(&[&k], &["CALLER_IDENTITY"]),
            &ServiceId::telephony(),
            RuntimeEnvironment::Production
        )
        .is_err());
        assert!(ok(&bundle(&[&k], &["CALLER_IDENTITY"])
            .replace("\"grant_kinds\"", "\"extra\": 1, \"grant_kinds\""))
        .is_err());
    }
}
