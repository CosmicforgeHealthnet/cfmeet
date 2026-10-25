//! # cf-service-auth — CF Meet internal service authentication
//!
//! Reusable, transport-independent implementation of the internal service authentication
//! protocol (Part B section 11) and the safe context types that cross service boundaries.
//! It is a shared package, not a deployable service, and it never depends on a deployable
//! service.
//!
//! Protocol summary (contracts in `packages/contracts/platform/`):
//!
//! * every service has its own Ed25519 key per runtime environment; tokens are JWS compact
//!   serializations (`alg` = EdDSA pinned, `typ` = `cf-service+jwt`, `kid`);
//! * claims bind issuer, exactly one audience, the runtime environment, a lifetime of at
//!   most 300 s (default 120 s), a token id, the hop's request id, the operation's
//!   correlation id and an optional [`DelegatedPlatformContext`];
//! * tokens travel in `X-CF-Service-Authorization`, never in the customer `Authorization`
//!   header; `X-CF-Request-Id` / `X-CF-Correlation-Id` must equal the signed claims;
//! * the receiver's [`TrustPolicy`] (trust bundle) decides which issuers and keys it
//!   accepts and which [`InternalPermission`]s it grants: tokens never carry permissions;
//! * key rotation is an overlap: receivers trust old + new keys, the sender switches, and
//!   the old key is removed after `max TTL + skew`.
//!
//! Distinct trust statements, distinct types, no conversions between them:
//! [`InternalServiceContext`] (which backend service called) vs
//! [`DelegatedPlatformContext`] (safe customer identifiers asserted by that service) —
//! and, inside each service, its own customer and participant identity types.

pub mod claims;
pub mod clock;
pub mod contact_delegation;
pub mod context;
pub mod delegated;
pub mod headers;
pub mod identity;
pub mod ids;
pub mod jwt;
pub mod keys;
pub mod number_grant;
pub mod permissions;
pub mod signer;
pub mod trust;
pub mod verifier;

#[cfg(feature = "client")]
pub mod client;

pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use contact_delegation::{
    ContactAccess, ContactAccessToken, ContactDelegationFailure, ContactDelegationSigner,
    ContactDelegationTrust, ContactDelegationVerifier, ContactScope,
};
pub use context::{InternalServiceContext, PermissionDenied};
pub use delegated::{ApplicationEnvironmentKind, DelegatedPlatformContext, DelegatedPrincipalKind};
pub use identity::{services, RuntimeEnvironment, ServiceId};
pub use ids::{CorrelationId, KeyId, RequestId, TokenId};
pub use keys::{PublicJwk, ServiceSigningKey};
pub use permissions::{InternalPermission, PermissionSet};
pub use signer::{
    OutboundServiceRequest, ServiceToken, ServiceTokenSigner, SignError, SignedServiceRequest,
    TokenTtl,
};
pub use trust::{TrustBundleError, TrustPolicy};
pub use verifier::{
    Diagnostics, PresentedServiceRequest, ServiceTokenVerifier, VerificationError,
    VerificationFailure,
};
