//! Canonical internal HTTP headers (contract `cfmeet.platform.internal-http` v1).
//!
//! Internal requests use a DEDICATED authorization header so a service token can never be
//! confused with a customer `Authorization: Bearer cfmk_…` credential, in either
//! direction.

use http::{header::AUTHORIZATION, HeaderMap, HeaderName};

/// `X-CF-Service-Authorization: Bearer <service token>`.
pub const SERVICE_AUTHORIZATION: HeaderName = HeaderName::from_static("x-cf-service-authorization");
/// `X-CF-Request-Id`: id of this hop; must equal the signed `request_id` claim.
pub const REQUEST_ID: HeaderName = HeaderName::from_static("x-cf-request-id");
/// `X-CF-Correlation-Id`: id of the causal operation; must equal the signed claim.
pub const CORRELATION_ID: HeaderName = HeaderName::from_static("x-cf-correlation-id");

pub const BEARER_SCHEME: &str = "Bearer";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderFault {
    Missing,
    Duplicate,
    Malformed,
}

/// Exactly one, visible-ASCII header value.
pub(crate) fn single<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
) -> Result<&'a str, HeaderFault> {
    let mut values = headers.get_all(name).iter();
    let first = values.next().ok_or(HeaderFault::Missing)?;
    if values.next().is_some() {
        return Err(HeaderFault::Duplicate);
    }
    first.to_str().map_err(|_| HeaderFault::Malformed)
}

/// Whether the request also carries a customer credential channel.
pub(crate) fn has_customer_authorization(headers: &HeaderMap) -> bool {
    headers.contains_key(AUTHORIZATION)
}

/// Token from `Bearer <token>` (scheme case-insensitive, exactly one space).
pub(crate) fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case(BEARER_SCHEME) && !token.is_empty() && !token.contains(' '))
        .then_some(token)
}
