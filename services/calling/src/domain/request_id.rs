//! Request / correlation identifiers.
//!
//! The authoritative [`RequestId`] and [`CorrelationId`] are always server-controlled
//! (canonical formats from `cf-service-auth`, contract `cfmeet.platform.common`). A
//! client-supplied correlation id is accepted only as a bounded, charset-restricted hint
//! ([`ClientCorrelationId`]) and never becomes either identifier.

pub use cf_service_auth::{CorrelationId, RequestId};

/// Server-controlled identifiers of the request being served.
///
/// * `request_id`: this request / hop (new for every hop);
/// * `correlation_id`: the causal operation. Generated when an operation enters CF Meet
///   through this service; for authenticated internal requests it is the signed
///   correlation id of the upstream hop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub correlation_id: CorrelationId,
}

impl RequestContext {
    /// A request that starts a new operation in this service.
    pub fn generate() -> Self {
        Self {
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
        }
    }
}

/// Client correlation hint from `x-request-id`: at most 64 characters of
/// `[A-Za-z0-9._-]`. Anything else is discarded, never truncated or escaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCorrelationId(String);

impl ClientCorrelationId {
    pub const MAX_LEN: usize = 64;

    pub fn parse(value: &str) -> Option<Self> {
        let valid = !value.is_empty()
            && value.len() <= Self::MAX_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        valid.then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_ids_are_unique_and_prefixed() {
        let a = RequestContext::generate();
        let b = RequestContext::generate();
        assert!(a.request_id.as_str().starts_with("req_"));
        assert_eq!(a.request_id.as_str().len(), 36);
        assert!(a.correlation_id.as_str().starts_with("corr_"));
        assert_ne!(a.request_id, b.request_id);
        assert_ne!(a.correlation_id, b.correlation_id);
    }

    #[test]
    fn client_correlation_ids_are_bounded_and_charset_restricted() {
        assert!(ClientCorrelationId::parse("abc-123_x.y").is_some());
        assert!(ClientCorrelationId::parse("").is_none());
        assert!(ClientCorrelationId::parse("has space").is_none());
        assert!(ClientCorrelationId::parse("new\nline").is_none());
        assert!(ClientCorrelationId::parse("{\"json\":1}").is_none());
        assert!(ClientCorrelationId::parse(&"x".repeat(64)).is_some());
        assert!(ClientCorrelationId::parse(&"x".repeat(65)).is_none());
    }
}
