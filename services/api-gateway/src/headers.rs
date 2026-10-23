//! Client-supplied internal authority headers.
//!
//! Nothing a client sends is trusted as authority. The gateway builds every upstream
//! request from scratch (method, upstream path, validated query, JSON body, an optional
//! `Idempotency-Key` and its own signed service token) so no client header is ever copied
//! upstream. [`stripped_authority_headers`] names the headers a client tried to assert, for
//! metrics and logs (names only, never values); a client `X-CF-Service-Authorization` is
//! refused outright (a service token never authenticates a customer).

use axum::http::HeaderMap;

/// Header name prefixes / names that claim internal authority or identity.
const AUTHORITY_PREFIXES: &[&str] = &[
    "x-cf-",
    "x-tenant",
    "x-application",
    "x-environment",
    "x-project",
    "x-principal",
    "x-service-account",
    "x-forwarded-",
    "x-internal",
    "x-operator",
];
const AUTHORITY_NAMES: &[&str] = &["forwarded", "x-real-ip", "x-user-id", "x-scopes"];

/// The client header forwarded on the Developer Platform foundation route (internal client).
pub const FORWARDED_CLIENT_HEADERS: &[&str] = &["idempotency-key"];

/// Client request headers forwarded on every public route (content negotiation,
/// idempotency and conditional requests). Everything else is dropped; a route may add a
/// documented header of its own (`forwardHeaders` in the manifest).
pub const FORWARDED_REQUEST_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "content-type",
    "idempotency-key",
    "if-match",
    "if-none-match",
    "range",
];

/// Upstream response headers relayed to the client (besides the gateway's own ids and
/// quota headers). Internal headers (service tokens, upstream request ids) never leave.
pub const RELAYED_RESPONSE_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "cache-control",
    "etag",
    "last-modified",
    "location",
    "retry-after",
    "idempotent-replayed",
    "content-disposition",
    "content-range",
    "accept-ranges",
    "deprecation",
    "sunset",
    "link",
    "www-authenticate",
    "ratelimit-limit",
    "ratelimit-remaining",
    "ratelimit-reset",
];

/// Bounded, visible-ASCII header value.
pub fn safe_value(v: &axum::http::HeaderValue) -> bool {
    v.len() <= 4096 && v.as_bytes().iter().all(|b| (0x20..=0x7e).contains(b))
}

/// Lower-case names of client headers that claim authority (all of them are dropped).
pub fn stripped_authority_headers(headers: &HeaderMap) -> Vec<String> {
    let mut names: Vec<String> = headers
        .keys()
        .map(|k| k.as_str().to_ascii_lowercase())
        .filter(|k| {
            AUTHORITY_PREFIXES.iter().any(|p| k.starts_with(p))
                || AUTHORITY_NAMES.contains(&k.as_str())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Whether the client presented a service token (refused at the ingress).
pub fn presents_service_token(headers: &HeaderMap) -> bool {
    headers.contains_key(cf_service_auth::headers::SERVICE_AUTHORIZATION)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn authority_claims_are_recognised_and_ordinary_headers_are_not() {
        let mut h = HeaderMap::new();
        for name in [
            "x-cf-request-id",
            "x-cf-delegated-context",
            "x-tenant-id",
            "x-environment-id",
            "x-forwarded-for",
            "forwarded",
            "accept",
            "content-type",
            "idempotency-key",
        ] {
            h.insert(
                axum::http::HeaderName::from_static(name),
                HeaderValue::from_static("v"),
            );
        }
        assert_eq!(
            stripped_authority_headers(&h),
            [
                "forwarded",
                "x-cf-delegated-context",
                "x-cf-request-id",
                "x-environment-id",
                "x-forwarded-for",
                "x-tenant-id"
            ]
        );
        assert!(!presents_service_token(&h));
        h.insert(
            "x-cf-service-authorization",
            HeaderValue::from_static("Bearer x"),
        );
        assert!(presents_service_token(&h));
    }
}
