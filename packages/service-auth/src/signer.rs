//! Outbound service token signing.
//!
//! A signer belongs to exactly one service identity in one runtime environment and holds
//! that service's own Ed25519 key. It mints a short-lived token for exactly one audience,
//! binding the hop's request id, the operation's correlation id and (optionally) a safe
//! delegated platform context. It accepts no permissions: what the caller may do is
//! decided by the receiver.

use std::fmt;

use http::{HeaderMap, HeaderValue};
use jsonwebtoken::{Algorithm, Header};
use secrecy::{ExposeSecret, SecretString};

use crate::{
    claims::{
        ServiceTokenClaims, CLAIMS_VERSION, DEFAULT_TTL_SECONDS, MAX_TTL_SECONDS, MIN_TTL_SECONDS,
        TOKEN_TYPE,
    },
    clock::SharedClock,
    context::InternalServiceContext,
    delegated::DelegatedPlatformContext,
    headers,
    identity::{RuntimeEnvironment, ServiceId},
    ids::{CorrelationId, RequestId, TokenId},
    keys::ServiceSigningKey,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    #[error("a service cannot address a token to itself ({0})")]
    SelfAudience(ServiceId),
    #[error("token lifetime must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS} seconds")]
    InvalidTtl,
    #[error("token signing failed")]
    Signing,
}

/// Validated token lifetime (10–300 s, default 120 s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenTtl(u64);

impl TokenTtl {
    pub fn seconds(seconds: u64) -> Result<Self, SignError> {
        if (MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&seconds) {
            Ok(Self(seconds))
        } else {
            Err(SignError::InvalidTtl)
        }
    }

    pub fn get(&self) -> u64 {
        self.0
    }
}

impl Default for TokenTtl {
    fn default() -> Self {
        Self(DEFAULT_TTL_SECONDS)
    }
}

/// Raw service token. `Debug` is redacted; there is no `Display` / `Serialize`.
pub struct ServiceToken(SecretString);

impl ServiceToken {
    /// The compact JWS, for placing into the outbound header only.
    pub fn expose_for_transport(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for ServiceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceToken([REDACTED])")
    }
}

/// What an outbound internal request needs: one audience, a NEW request id for this hop,
/// the operation's correlation id, and optionally a delegated context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundServiceRequest {
    audience: ServiceId,
    request_id: RequestId,
    correlation_id: CorrelationId,
    delegated_context: Option<DelegatedPlatformContext>,
}

impl OutboundServiceRequest {
    /// A hop belonging to an existing operation: new request id, same correlation id.
    pub fn continuing(audience: ServiceId, correlation_id: &CorrelationId) -> Self {
        Self {
            audience,
            request_id: RequestId::generate(),
            correlation_id: correlation_id.clone(),
            delegated_context: None,
        }
    }

    /// The next hop of an authenticated internal request: new request id, same
    /// correlation id, the same delegated context (re-asserted by this service).
    pub fn next_hop_of(inbound: &InternalServiceContext, audience: ServiceId) -> Self {
        Self {
            audience,
            request_id: RequestId::generate(),
            correlation_id: inbound.correlation_id().clone(),
            delegated_context: inbound.delegated_platform_context().cloned(),
        }
    }

    /// A hop of an operation that starts in this service (no inbound request): new
    /// request id and new correlation id.
    pub fn new_operation(audience: ServiceId) -> Self {
        Self::continuing(audience, &CorrelationId::generate())
    }

    pub fn with_delegated_context(mut self, context: DelegatedPlatformContext) -> Self {
        self.delegated_context = Some(context);
        self
    }

    pub fn audience(&self) -> &ServiceId {
        &self.audience
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }

    pub fn delegated_context(&self) -> Option<&DelegatedPlatformContext> {
        self.delegated_context.as_ref()
    }
}

/// A signed outbound request: token plus the id headers it is bound to.
#[derive(Debug)]
pub struct SignedServiceRequest {
    token: ServiceToken,
    audience: ServiceId,
    request_id: RequestId,
    correlation_id: CorrelationId,
}

impl SignedServiceRequest {
    pub fn audience(&self) -> &ServiceId {
        &self.audience
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn correlation_id(&self) -> &CorrelationId {
        &self.correlation_id
    }

    pub fn token(&self) -> &ServiceToken {
        &self.token
    }

    /// Writes the three internal headers. The authorization value is marked sensitive
    /// (HTTP libraries redact sensitive values in their debug output).
    pub fn apply_to(&self, target: &mut HeaderMap) {
        let mut auth = HeaderValue::from_str(&format!(
            "{} {}",
            headers::BEARER_SCHEME,
            self.token.expose_for_transport()
        ))
        .expect("compact JWS is a valid header value");
        auth.set_sensitive(true);
        target.insert(headers::SERVICE_AUTHORIZATION, auth);
        target.insert(
            headers::REQUEST_ID,
            HeaderValue::from_str(self.request_id.as_str()).expect("ids are ASCII"),
        );
        target.insert(
            headers::CORRELATION_ID,
            HeaderValue::from_str(self.correlation_id.as_str()).expect("ids are ASCII"),
        );
    }

    pub fn headers(&self) -> HeaderMap {
        let mut map = HeaderMap::new();
        self.apply_to(&mut map);
        map
    }
}

pub struct ServiceTokenSigner {
    issuer: ServiceId,
    runtime_environment: RuntimeEnvironment,
    key: ServiceSigningKey,
    ttl: TokenTtl,
    clock: SharedClock,
}

impl fmt::Debug for ServiceTokenSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceTokenSigner")
            .field("issuer", &self.issuer)
            .field("runtime_environment", &self.runtime_environment)
            .field("key", &self.key)
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl ServiceTokenSigner {
    pub fn new(
        issuer: ServiceId,
        runtime_environment: RuntimeEnvironment,
        key: ServiceSigningKey,
        ttl: TokenTtl,
        clock: SharedClock,
    ) -> Self {
        Self {
            issuer,
            runtime_environment,
            key,
            ttl,
            clock,
        }
    }

    pub fn issuer(&self) -> &ServiceId {
        &self.issuer
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime_environment
    }

    pub fn key_id(&self) -> &crate::ids::KeyId {
        self.key.kid()
    }

    pub fn ttl(&self) -> TokenTtl {
        self.ttl
    }

    pub fn sign(
        &self,
        request: &OutboundServiceRequest,
    ) -> Result<SignedServiceRequest, SignError> {
        if request.audience == self.issuer {
            return Err(SignError::SelfAudience(self.issuer.clone()));
        }
        let now = self.clock.now_unix();
        let claims = ServiceTokenClaims {
            ver: CLAIMS_VERSION,
            iss: self.issuer.clone(),
            aud: request.audience.clone(),
            runtime_environment: self.runtime_environment,
            iat: now,
            nbf: now,
            exp: now + self.ttl.get() as i64,
            jti: TokenId::generate(),
            request_id: request.request_id.clone(),
            correlation_id: request.correlation_id.clone(),
            delegated_context: request.delegated_context.clone(),
        };
        let header = Header {
            typ: Some(TOKEN_TYPE.to_string()),
            kid: Some(self.key.kid().as_str().to_string()),
            ..Header::new(Algorithm::EdDSA)
        };
        let token = jsonwebtoken::encode(&header, &claims, self.key.encoding_key())
            .map_err(|_| SignError::Signing)?;
        Ok(SignedServiceRequest {
            token: ServiceToken(SecretString::from(token)),
            audience: request.audience.clone(),
            request_id: request.request_id.clone(),
            correlation_id: request.correlation_id.clone(),
        })
    }
}
