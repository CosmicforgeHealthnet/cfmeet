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
//! 6. the id headers equal the signed `request_id` / `correlation_id` claims;
//! 7. ([`ServiceTokenVerifier::verify_request`], SEC-012) the optional request binding
//!    (`htm` / `htu`, [`crate::binding`]) matches the request per the receiver's
//!    [`RequestBindingMode`], and a token on a mutating request is single-use
//!    ([`crate::replay`]).
//!
//! Permissions are then taken from the receiver's policy for the issuer — never from the
//! token. Callers learn only "authentication failed"; [`VerificationFailure`] and the
//! safe [`Diagnostics`] are for operator telemetry.

use std::{fmt, sync::Arc};

use http::{HeaderMap, Method};
use jsonwebtoken::{errors::ErrorKind, Algorithm, Validation};

use crate::{
    binding::{canonical_request_path, is_mutating_method, RequestBindingMode},
    claims::{
        parse_protected_header, split_compact, HeaderProblem, ServiceTokenClaims, CLAIMS_VERSION,
        MAX_CLOCK_SKEW_SECONDS, MAX_TTL_SECONDS,
    },
    clock::SharedClock,
    context::InternalServiceContext,
    headers::{self, HeaderFault},
    identity::{RuntimeEnvironment, ServiceId},
    ids::{CorrelationId, KeyId, RequestId},
    replay::{
        replay_guard_enabled_from_env, InMemoryReplayGuard, ReplayGuard, ReplayKey, ReplayVerdict,
    },
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
    /// `require` mode: the token carries no request binding.
    RequestBindingMissing,
    /// `require` mode: verified without the request (`verify_headers`); use
    /// `verify_request`.
    RequestBindingUnchecked,
    /// The signed `htm` differs from the request method.
    RequestMethodMismatch,
    /// The signed `htu` differs from the canonical request path.
    RequestPathMismatch,
    /// A mutating request presented a token that was already used.
    Replayed,
    /// The replay store could not be consulted (fail closed).
    ReplayStoreUnavailable,
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
            RequestBindingMissing => "request_binding_missing",
            RequestBindingUnchecked => "request_binding_unchecked",
            RequestMethodMismatch => "request_method_mismatch",
            RequestPathMismatch => "request_path_mismatch",
            Replayed => "replayed_token",
            ReplayStoreUnavailable => "replay_store_unavailable",
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
    binding_mode: RequestBindingMode,
    replay_guard: Option<Arc<dyn ReplayGuard>>,
}

impl fmt::Debug for ServiceTokenVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceTokenVerifier")
            .field("policy", &self.policy)
            .field("max_lifetime", &self.max_lifetime)
            .field("skew", &self.skew)
            .field("binding_mode", &self.binding_mode)
            .field("replay_guard", &self.replay_guard.is_some())
            .finish()
    }
}

/// The HTTP request a token is presented on (method and request-target path, without
/// query; the receiver's own view of the request).
#[derive(Debug, Clone, Copy)]
struct RequestTarget<'a> {
    method: &'a Method,
    path: &'a str,
}

impl ServiceTokenVerifier {
    /// Verifier for the receiver and runtime environment of `policy`, accepting tokens
    /// with a lifetime of at most 300 s and 30 s of clock skew.
    ///
    /// Request binding mode from `SERVICE_AUTH_REQUEST_BINDING_MODE` (`accept` by
    /// default); jti replay guard per `SERVICE_AUTH_REPLAY_GUARD` (`on` by default: the
    /// process-wide [`InMemoryReplayGuard`]). An invalid value is a startup configuration
    /// error (panics naming the variable).
    pub fn new(policy: TrustPolicy, clock: SharedClock) -> Self {
        let binding_mode = RequestBindingMode::from_env().unwrap_or_else(|e| panic!("{e}"));
        let replay_guard = replay_guard_enabled_from_env()
            .unwrap_or_else(|e| panic!("{e}"))
            .then(|| InMemoryReplayGuard::process_default() as Arc<dyn ReplayGuard>);
        Self {
            policy,
            clock,
            max_lifetime: MAX_TTL_SECONDS,
            skew: MAX_CLOCK_SKEW_SECONDS,
            binding_mode,
            replay_guard,
        }
    }

    /// Explicit request binding mode (overrides the environment).
    pub fn with_binding_mode(mut self, mode: RequestBindingMode) -> Self {
        self.binding_mode = mode;
        self
    }

    pub fn binding_mode(&self) -> RequestBindingMode {
        self.binding_mode
    }

    /// Replaces the replay guard (e.g. a store shared by all replicas).
    pub fn with_replay_guard(mut self, guard: Arc<dyn ReplayGuard>) -> Self {
        self.replay_guard = Some(guard);
        self
    }

    /// Disables the jti replay guard (tests; or a receiver with its own durable check).
    pub fn without_replay_guard(mut self) -> Self {
        self.replay_guard = None;
        self
    }

    pub fn has_replay_guard(&self) -> bool {
        self.replay_guard.is_some()
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

    /// Verifies the internal headers of an HTTP request WITHOUT the request itself: no
    /// request binding comparison and no replay guard. Refused in
    /// [`RequestBindingMode::Require`] (`request_binding_unchecked`). HTTP receivers use
    /// [`Self::verify_request`].
    pub fn verify_headers(
        &self,
        map: &HeaderMap,
    ) -> Result<InternalServiceContext, VerificationError> {
        let presented = Self::presented(map)?;
        self.verify_inner(&presented, None)
    }

    /// Verifies the service token of an HTTP request (SEC-012): everything
    /// [`Self::verify_headers`] checks, plus
    ///
    /// * the request binding per the [`RequestBindingMode`]: `htm` must equal `method`
    ///   and `htu` the canonical form of `path` (the request-target path the receiver
    ///   routed, without query) — `accept` checks them when present, `require` also
    ///   refuses tokens without them, `off` ignores them (they must still be
    ///   well-formed);
    /// * for mutating methods (`POST`, `PUT`, `PATCH`, `DELETE`), single use of the
    ///   token's `jti` through the [`ReplayGuard`].
    pub async fn verify_request(
        &self,
        method: &Method,
        path: &str,
        map: &HeaderMap,
    ) -> Result<InternalServiceContext, VerificationError> {
        let context = self.verify_request_binding(method, path, map)?;
        self.admit_once(method, &context).await?;
        Ok(context)
    }

    /// The replay step of [`Self::verify_request`] on its own: for a mutating `method`,
    /// records the verified token's `(issuer, jti)` and refuses a second use. For callers
    /// that verified synchronously with [`Self::verify_request_binding`] (e.g. inside a
    /// non-async helper) and admit the request afterwards. A no-op for safe methods or
    /// without a replay guard.
    pub async fn admit_once(
        &self,
        method: &Method,
        context: &InternalServiceContext,
    ) -> Result<(), VerificationError> {
        if !is_mutating_method(method) {
            return Ok(());
        }
        let Some(guard) = &self.replay_guard else {
            return Ok(());
        };
        let key = ReplayKey {
            issuer: context.authenticated_service.clone(),
            token_id: context.token_id.clone(),
        };
        let retain_until = context.expires_at.saturating_add(self.skew as i64);
        let fail = |failure| VerificationError {
            failure,
            diagnostics: Diagnostics {
                kid: Some(context.key_id.clone()),
                issuer: Some(context.authenticated_service.clone()),
                request_id: Some(context.request_id.clone()),
                correlation_id: Some(context.correlation_id.clone()),
            },
        };
        match guard
            .check_and_record(&key, retain_until, self.clock.now_unix())
            .await
        {
            Ok(ReplayVerdict::FirstUse) => Ok(()),
            Ok(ReplayVerdict::Replayed) => Err(fail(VerificationFailure::Replayed)),
            Err(_) => Err(fail(VerificationFailure::ReplayStoreUnavailable)),
        }
    }

    /// [`Self::verify_request`] without the replay guard (request binding only), for
    /// synchronous callers; they MUST then call [`Self::admit_once`] before acting.
    pub fn verify_request_binding(
        &self,
        method: &Method,
        path: &str,
        map: &HeaderMap,
    ) -> Result<InternalServiceContext, VerificationError> {
        let presented = Self::presented(map)?;
        self.verify_inner(&presented, Some(RequestTarget { method, path }))
    }

    fn presented(map: &HeaderMap) -> Result<PresentedServiceRequest<'_>, VerificationError> {
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
        Ok(PresentedServiceRequest {
            service_authorization,
            request_id,
            correlation_id,
        })
    }

    /// Verifies raw presented values without the request (same rules as
    /// [`Self::verify_headers`]).
    pub fn verify(
        &self,
        presented: &PresentedServiceRequest<'_>,
    ) -> Result<InternalServiceContext, VerificationError> {
        self.verify_inner(presented, None)
    }

    fn verify_inner(
        &self,
        presented: &PresentedServiceRequest<'_>,
        target: Option<RequestTarget<'_>>,
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
            match crate::jwt::decode::<serde_json::Value>(token, &trusted.key, &validation) {
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

        // Request binding (SEC-012). Claim well-formedness (all-or-nothing group, `bnd`,
        // upper-case `htm`, canonical `htu`) was enforced when parsing, in every mode.
        let binding = claims.request_binding();
        match (self.binding_mode, target) {
            (RequestBindingMode::Off, _) => {}
            (RequestBindingMode::Accept, None) => {}
            (RequestBindingMode::Require, None) => {
                fail!(VerificationFailure::RequestBindingUnchecked)
            }
            (mode, Some(target)) => match &binding {
                None if mode == RequestBindingMode::Require => {
                    fail!(VerificationFailure::RequestBindingMissing)
                }
                None => {}
                Some(bound) => {
                    if bound.method() != target.method.as_str() {
                        fail!(VerificationFailure::RequestMethodMismatch)
                    }
                    match canonical_request_path(target.path) {
                        Ok(path) if path == bound.path() => {}
                        _ => fail!(VerificationFailure::RequestPathMismatch),
                    }
                }
            },
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
            request_bound: binding.is_some()
                && target.is_some()
                && self.binding_mode != RequestBindingMode::Off,
        })
    }
}
