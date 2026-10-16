//! Inbound service token verification.
//!
//! Network location is never authentication: every internal request must present a
//! service token, whatever interface it arrived on. Verification order:
//!
//! 1. exactly one `X-CF-Service-Authorization: Bearer <token>`, `X-CF-Request-Id` and
//!    `X-CF-Correlation-Id`; no customer `Authorization` header;
//! 2. compact JWS shape and size; strict protected header `{alg: EdDSA, typ:
//!    cf-service+jwt, kid}` — no algorithm choice, no embedded or remote keys;
//! 3. `kid` resolves to a key of a trusted issuer in THIS service's trust bundle;
//! 4. Ed25519 signature verification (`jsonwebtoken`, algorithm pinned to EdDSA);
//! 5. closed claim set (unknown claims, e.g. self-declared permissions, are rejected);
//!    `ver` = 1; `iss` = the issuer bound to the key; `aud` = this service exactly;
//!    `runtime_environment` = this service's environment exactly; `iat <= nbf < exp`,
//!    `exp - iat <= 300 s` (and <= the configured maximum), not before `nbf`, not after
//!    `exp`, with at most 30 s skew;
//! 6. the id headers equal the signed `request_id` / `correlation_id` claims.
//!
//! Permissions are then taken from the receiver's policy for the issuer — never from the
//! token. Callers learn only "authentication failed"; [`VerificationFailure`] and the
//! safe [`Diagnostics`] are for operator telemetry.

use std::fmt;

use http::HeaderMap;
use jsonwebtoken::{errors::ErrorKind, Algorithm, Validation};

use crate::{
    claims::{
        parse_protected_header, split_compact, HeaderProblem, ServiceTokenClaims, CLAIMS_VERSION,
        MAX_CLOCK_SKEW_SECONDS, MAX_TTL_SECONDS,
    },
    clock::SharedClock,
    context::InternalServiceContext,
    headers::{self, HeaderFault},
    identity::{RuntimeEnvironment, ServiceId},
    ids::{CorrelationId, KeyId, RequestId},
    trust::TrustPolicy,
};

/// Why verification failed (operator telemetry only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFailure {
    MissingServiceToken,
    DuplicateServiceToken,
    MalformedAuthorization,
    CustomerCredentialPresent,
    MissingRequestId,
    MissingCorrelationId,
    DuplicateIdHeader,
    MalformedRequestId,
    MalformedCorrelationId,
    MalformedToken,
    UnsupportedAlgorithm,
    UnsupportedTokenType,
    ForbiddenHeaderParameter,
    UnknownKey,
    BadSignature,
    MalformedClaims,
    UnsupportedVersion,
    IssuerKeyMismatch,
    AudienceMismatch,
    RuntimeEnvironmentMismatch,
    InvalidTimeClaims,
    IssuedInFuture,
    NotYetValid,
    Expired,
    LifetimeTooLong,
    RequestIdMismatch,
    CorrelationIdMismatch,
}

impl VerificationFailure {
    pub fn as_str(&self) -> &'static str {
        use VerificationFailure::*;
        match self {
            MissingServiceToken => "missing_service_token",
            DuplicateServiceToken => "duplicate_service_token",
            MalformedAuthorization => "malformed_service_authorization",
            CustomerCredentialPresent => "customer_credential_present",
            MissingRequestId => "missing_request_id",
            MissingCorrelationId => "missing_correlation_id",
            DuplicateIdHeader => "duplicate_id_header",
            MalformedRequestId => "malformed_request_id",
            MalformedCorrelationId => "malformed_correlation_id",
            MalformedToken => "malformed_token",
            UnsupportedAlgorithm => "unsupported_algorithm",
            UnsupportedTokenType => "unsupported_token_type",
            ForbiddenHeaderParameter => "forbidden_header_parameter",
            UnknownKey => "unknown_key",
            BadSignature => "bad_signature",
            MalformedClaims => "malformed_claims",
            UnsupportedVersion => "unsupported_version",
            IssuerKeyMismatch => "issuer_key_mismatch",
            AudienceMismatch => "audience_mismatch",
            RuntimeEnvironmentMismatch => "runtime_environment_mismatch",
            InvalidTimeClaims => "invalid_time_claims",
            IssuedInFuture => "issued_in_future",
            NotYetValid => "not_yet_valid",
            Expired => "expired",
            LifetimeTooLong => "lifetime_too_long",
            RequestIdMismatch => "request_id_mismatch",
            CorrelationIdMismatch => "correlation_id_mismatch",
        }
    }
}

/// Safe, already-validated values for operator logs. Never token contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub kid: Option<KeyId>,
    /// Issuer bound to a trusted key (known only once the key resolved).
    pub issuer: Option<ServiceId>,
    pub request_id: Option<RequestId>,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationError {
    pub failure: VerificationFailure,
    pub diagnostics: Diagnostics,
}

impl fmt::Display for VerificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "internal service authentication failed ({})",
            self.failure.as_str()
        )
    }
}

impl std::error::Error for VerificationError {}

/// The raw values of one inbound request.
#[derive(Clone, Copy)]
pub struct PresentedServiceRequest<'a> {
    /// Full `X-CF-Service-Authorization` header value (`Bearer <token>`).
    pub service_authorization: &'a str,
    pub request_id: &'a str,
    pub correlation_id: &'a str,
}

impl fmt::Debug for PresentedServiceRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedServiceRequest")
            .field("service_authorization", &"[REDACTED]")
            .field("request_id", &crate::identity::bounded(self.request_id))
            .field(
                "correlation_id",
                &crate::identity::bounded(self.correlation_id),
            )
            .finish()
    }
}

pub struct ServiceTokenVerifier {
    policy: TrustPolicy,
    clock: SharedClock,
    max_lifetime: u64,
    skew: u64,
}

impl fmt::Debug for ServiceTokenVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceTokenVerifier")
            .field("policy", &self.policy)
            .field("max_lifetime", &self.max_lifetime)
            .field("skew", &self.skew)
            .finish()
    }
}

impl ServiceTokenVerifier {
    /// Verifier for the receiver and runtime environment of `policy`, accepting tokens
    /// with a lifetime of at most 300 s and 30 s of clock skew.
    pub fn new(policy: TrustPolicy, clock: SharedClock) -> Self {
        Self {
            policy,
            clock,
            max_lifetime: MAX_TTL_SECONDS,
            skew: MAX_CLOCK_SKEW_SECONDS,
        }
    }

    /// Tighter limits (never looser than the protocol maximums).
    pub fn with_limits(mut self, max_lifetime_seconds: u64, skew_seconds: u64) -> Self {
        self.max_lifetime = max_lifetime_seconds.min(MAX_TTL_SECONDS);
        self.skew = skew_seconds.min(MAX_CLOCK_SKEW_SECONDS);
        self
    }

    pub fn receiver(&self) -> &ServiceId {
        self.policy.receiver()
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.policy.runtime_environment()
    }

    pub fn policy(&self) -> &TrustPolicy {
        &self.policy
    }

    /// Verifies the internal headers of an HTTP request.
    pub fn verify_headers(
        &self,
        map: &HeaderMap,
    ) -> Result<InternalServiceContext, VerificationError> {
        let fail = |failure| VerificationError {
            failure,
            diagnostics: Diagnostics::default(),
        };
        if headers::has_customer_authorization(map) {
            return Err(fail(VerificationFailure::CustomerCredentialPresent));
        }
        let service_authorization =
            headers::single(map, &headers::SERVICE_AUTHORIZATION).map_err(|f| {
                fail(match f {
                    HeaderFault::Missing => VerificationFailure::MissingServiceToken,
                    HeaderFault::Duplicate => VerificationFailure::DuplicateServiceToken,
                    HeaderFault::Malformed => VerificationFailure::MalformedAuthorization,
                })
            })?;
        let request_id = headers::single(map, &headers::REQUEST_ID).map_err(|f| {
            fail(match f {
                HeaderFault::Missing => VerificationFailure::MissingRequestId,
                HeaderFault::Duplicate => VerificationFailure::DuplicateIdHeader,
                HeaderFault::Malformed => VerificationFailure::MalformedRequestId,
            })
        })?;
        let correlation_id = headers::single(map, &headers::CORRELATION_ID).map_err(|f| {
            fail(match f {
                HeaderFault::Missing => VerificationFailure::MissingCorrelationId,
                HeaderFault::Duplicate => VerificationFailure::DuplicateIdHeader,
                HeaderFault::Malformed => VerificationFailure::MalformedCorrelationId,
            })
        })?;
        self.verify(&PresentedServiceRequest {
            service_authorization,
            request_id,
            correlation_id,
        })
    }

    pub fn verify(
        &self,
        presented: &PresentedServiceRequest<'_>,
    ) -> Result<InternalServiceContext, VerificationError> {
        let mut diagnostics = Diagnostics::default();
        macro_rules! fail {
            ($failure:expr) => {
                return Err(VerificationError {
                    failure: $failure,
                    diagnostics,
                })
            };
        }

        // Id headers: server-generated formats only.
        let Ok(request_id) = RequestId::parse(presented.request_id) else {
            fail!(VerificationFailure::MalformedRequestId)
        };
        diagnostics.request_id = Some(request_id.clone());
        let Ok(correlation_id) = CorrelationId::parse(presented.correlation_id) else {
            fail!(VerificationFailure::MalformedCorrelationId)
        };
        diagnostics.correlation_id = Some(correlation_id.clone());

        // Token shape and strict header.
        let Some(token) = headers::bearer_token(presented.service_authorization) else {
            fail!(VerificationFailure::MalformedAuthorization)
        };
        let Some((header_segment, _, _)) = split_compact(token) else {
            fail!(VerificationFailure::MalformedToken)
        };
        let header = match parse_protected_header(header_segment) {
            Ok(h) => h,
            Err(HeaderProblem::Malformed) => fail!(VerificationFailure::MalformedToken),
            Err(HeaderProblem::Algorithm) => fail!(VerificationFailure::UnsupportedAlgorithm),
            Err(HeaderProblem::TokenType) => fail!(VerificationFailure::UnsupportedTokenType),
            Err(HeaderProblem::ForbiddenParameter) => {
                fail!(VerificationFailure::ForbiddenHeaderParameter)
            }
        };
        diagnostics.kid = Some(header.kid.clone());

        // Key of a trusted issuer.
        let Some(trusted) = self.policy.key(&header.kid) else {
            fail!(VerificationFailure::UnknownKey)
        };
        diagnostics.issuer = Some(trusted.issuer.clone());

        // Signature (algorithm pinned). Claim checks are done below with our own clock.
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.algorithms = vec![Algorithm::EdDSA];
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.leeway = 0;
        let decoded =
            match jsonwebtoken::decode::<serde_json::Value>(token, &trusted.key, &validation) {
                Ok(d) => d,
                Err(e) => match e.kind() {
                    ErrorKind::InvalidSignature => fail!(VerificationFailure::BadSignature),
                    ErrorKind::InvalidAlgorithm => fail!(VerificationFailure::UnsupportedAlgorithm),
                    _ => fail!(VerificationFailure::MalformedToken),
                },
            };
        let claims: ServiceTokenClaims = match serde_json::from_value(decoded.claims) {
            Ok(c) => c,
            Err(_) => fail!(VerificationFailure::MalformedClaims),
        };

        if claims.ver != CLAIMS_VERSION {
            fail!(VerificationFailure::UnsupportedVersion)
        }
        if claims.iss != trusted.issuer {
            fail!(VerificationFailure::IssuerKeyMismatch)
        }
        if &claims.aud != self.policy.receiver() {
            fail!(VerificationFailure::AudienceMismatch)
        }
        if claims.runtime_environment != self.policy.runtime_environment() {
            fail!(VerificationFailure::RuntimeEnvironmentMismatch)
        }

        let now = self.clock.now_unix();
        let skew = self.skew as i64;
        if claims.iat < 0 || claims.nbf < claims.iat || claims.exp <= claims.nbf {
            fail!(VerificationFailure::InvalidTimeClaims)
        }
        if claims.exp - claims.iat > self.max_lifetime as i64 {
            fail!(VerificationFailure::LifetimeTooLong)
        }
        if claims.iat > now + skew {
            fail!(VerificationFailure::IssuedInFuture)
        }
        if now + skew < claims.nbf {
            fail!(VerificationFailure::NotYetValid)
        }
        if now - skew >= claims.exp {
            fail!(VerificationFailure::Expired)
        }

        if claims.request_id != request_id {
            fail!(VerificationFailure::RequestIdMismatch)
        }
        if claims.correlation_id != correlation_id {
            fail!(VerificationFailure::CorrelationIdMismatch)
        }

        let permissions = self
            .policy
            .grant_for(&claims.iss)
            .map(|g| g.permissions().clone())
            .unwrap_or_default();
        Ok(InternalServiceContext {
            authenticated_service: claims.iss,
            audience: claims.aud,
            runtime_environment: claims.runtime_environment,
            key_id: header.kid,
            token_id: claims.jti,
            issued_at: claims.iat,
            expires_at: claims.exp,
            request_id,
            correlation_id,
            permissions,
            delegated_platform_context: claims.delegated_context,
        })
    }
}
