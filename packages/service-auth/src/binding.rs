//! Request binding of service tokens (SEC-012): the optional `htm` (HTTP method), `htu`
//! (canonical request path) and `bnd` (binding version) claims.
//!
//! A bound token is valid for exactly one HTTP method on exactly one path of its audience,
//! so a token captured from one internal call cannot be re-aimed at another route of the
//! same receiver within its lifetime.
//!
//! # Claims
//!
//! * `bnd` — request binding version, currently [`REQUEST_BINDING_VERSION`] (`1`);
//! * `htm` — the request method, upper-case ASCII letters, 1–16 characters (`POST`);
//! * `htu` — the request path in canonical form (see below).
//!
//! The three claims are a group: a token carries all of them or none of them. A partial
//! group, an unknown `bnd`, a lower-case method or a non-canonical path makes the claim
//! set malformed in EVERY receiver mode (including `off`).
//!
//! # Canonical request path (`htu`)
//!
//! [`canonical_request_path`] defines the one canonical form the signer emits and the
//! receiver recomputes from the path it actually received:
//!
//! 1. **Path only.** No scheme, host, port, query (`?`) or fragment (`#`): the query is
//!    not bound (internal routes carry their authority-relevant inputs in the path or the
//!    body, and query strings are re-encoded by HTTP libraries).
//! 2. It starts with `/` and is at most [`MAX_REQUEST_PATH_LEN`] bytes after
//!    normalization.
//! 3. **Percent-encoding normalization** (RFC 3986 §6.2.2.1–2): a percent-escape of an
//!    unreserved character (`A–Z a–z 0–9 - . _ ~`) is decoded (`%7E` → `~`, `%41` → `A`);
//!    every other escape is kept with UPPER-case hex digits (`%2f` → `%2F`). An encoded
//!    slash therefore stays distinct from a path separator. A `%` not followed by two hex
//!    digits is refused.
//! 4. Only RFC 3986 `pchar`s and `/` may appear unencoded; anything else (space,
//!    non-ASCII, `"`, `<`, `\`, …) is refused rather than guessed at.
//! 5. **No dot or empty segments.** After decoding, a `.` or `..` segment (including
//!    `%2E%2E`) or an empty segment (`//`) is refused: such paths are never produced by
//!    the internal client and are not silently normalized.
//! 6. **The trailing slash is significant** (`/a/` ≠ `/a`) and the path is
//!    case-sensitive, exactly as the receivers' routers treat them.
//!
//! # Rollout
//!
//! Receivers ship first in [`RequestBindingMode::Accept`] (verify the claims when present),
//! then signers emit them ([`RequestBindingEmission::Emit`]), then receivers move to
//! [`RequestBindingMode::Require`]. See `docs/operations/service-token-request-binding.md`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The only request binding version.
pub const REQUEST_BINDING_VERSION: u8 = 1;
/// Maximum canonical request path length.
pub const MAX_REQUEST_PATH_LEN: usize = 1024;
/// Maximum `htm` length.
pub const MAX_METHOD_LEN: usize = 16;

/// Receiver binding mode (env [`RequestBindingMode::ENV`]).
pub const ENV_RECEIVER_MODE: &str = "SERVICE_AUTH_REQUEST_BINDING_MODE";
/// Signer emission switch (env [`RequestBindingEmission::ENV`]).
pub const ENV_SIGNER_EMISSION: &str = "SERVICE_AUTH_REQUEST_BINDING";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestPathError {
    #[error("request path is empty or not absolute")]
    NotAbsolute,
    #[error("request path is longer than {MAX_REQUEST_PATH_LEN} bytes")]
    TooLong,
    #[error("request path contains a query or fragment")]
    QueryOrFragment,
    #[error("request path contains a malformed percent-escape")]
    BadEscape,
    #[error("request path contains a character that must be percent-encoded")]
    BadCharacter,
    #[error("request path contains a dot or empty segment")]
    BadSegment,
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// RFC 3986 `pchar` minus percent-escapes, plus the segment separator.
fn is_plain_path_byte(b: u8) -> bool {
    is_unreserved(b)
        || matches!(
            b,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
        )
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The canonical form of a request path (module documentation, "Canonical request
/// path"). Signers bind this value; receivers recompute it from the received path.
pub fn canonical_request_path(path: &str) -> Result<String, RequestPathError> {
    let bytes = path.as_bytes();
    if bytes.first() != Some(&b'/') {
        return Err(RequestPathError::NotAbsolute);
    }
    // Normalization never lengthens a path, but refuse absurd inputs before work.
    if bytes.len() > MAX_REQUEST_PATH_LEN * 3 {
        return Err(RequestPathError::TooLong);
    }
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'%' => {
                let (Some(hi), Some(lo)) = (
                    bytes.get(i + 1).copied().and_then(hex_value),
                    bytes.get(i + 2).copied().and_then(hex_value),
                ) else {
                    return Err(RequestPathError::BadEscape);
                };
                let value = hi << 4 | lo;
                if is_unreserved(value) {
                    out.push(value as char);
                } else {
                    out.push('%');
                    out.push(char::from(b"0123456789ABCDEF"[usize::from(hi)]));
                    out.push(char::from(b"0123456789ABCDEF"[usize::from(lo)]));
                }
                i += 3;
            }
            b'?' | b'#' => return Err(RequestPathError::QueryOrFragment),
            b if is_plain_path_byte(b) => {
                out.push(b as char);
                i += 1;
            }
            _ => return Err(RequestPathError::BadCharacter),
        }
    }
    if out.len() > MAX_REQUEST_PATH_LEN {
        return Err(RequestPathError::TooLong);
    }
    let segments: Vec<&str> = out.split('/').skip(1).collect();
    let last = segments.len() - 1;
    for (n, segment) in segments.iter().enumerate() {
        let empty_inner = segment.is_empty() && n != last;
        if empty_inner || *segment == "." || *segment == ".." {
            return Err(RequestPathError::BadSegment);
        }
    }
    Ok(out)
}

/// Whether `method` is a valid `htm` value: 1–16 upper-case ASCII letters.
pub fn is_valid_method(method: &str) -> bool {
    !method.is_empty()
        && method.len() <= MAX_METHOD_LEN
        && method.bytes().all(|b| b.is_ascii_uppercase())
}

/// Mutating methods: their tokens are single-use (jti replay guard).
pub fn is_mutating_method(method: &http::Method) -> bool {
    matches!(
        *method,
        http::Method::POST | http::Method::PUT | http::Method::PATCH | http::Method::DELETE
    )
}

/// `htm` claim value (validated on deserialization).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MethodClaim(String);

impl TryFrom<String> for MethodClaim {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if is_valid_method(&value) {
            Ok(Self(value))
        } else {
            Err("htm must be 1-16 upper-case ASCII letters")
        }
    }
}

impl From<MethodClaim> for String {
    fn from(m: MethodClaim) -> String {
        m.0
    }
}

impl MethodClaim {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `htu` claim value: a path already in canonical form (validated on deserialization).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PathClaim(String);

impl TryFrom<String> for PathClaim {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match canonical_request_path(&value) {
            Ok(canonical) if canonical == value => Ok(Self(value)),
            _ => Err("htu must be a canonical request path"),
        }
    }
}

impl From<PathClaim> for String {
    fn from(p: PathClaim) -> String {
        p.0
    }
}

impl PathClaim {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A verified-well-formed request binding (`bnd` + `htm` + `htu`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestBinding {
    pub(crate) method: MethodClaim,
    pub(crate) path: PathClaim,
}

impl RequestBinding {
    /// Binding for `method` on `path` (canonicalized here).
    pub fn new(method: &http::Method, path: &str) -> Result<Self, RequestBindingError> {
        let method = MethodClaim::try_from(method.as_str().to_string())
            .map_err(|_| RequestBindingError::Method)?;
        let path = canonical_request_path(path).map_err(RequestBindingError::Path)?;
        Ok(Self {
            method,
            path: PathClaim(path),
        })
    }

    pub fn method(&self) -> &str {
        self.method.as_str()
    }

    pub fn path(&self) -> &str {
        self.path.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RequestBindingError {
    #[error("the request method cannot be bound")]
    Method,
    #[error("the request path cannot be bound: {0}")]
    Path(RequestPathError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid value for {variable}: expected one of {expected}")]
pub struct BindingConfigError {
    pub variable: &'static str,
    pub expected: &'static str,
}

/// How a receiver treats the request binding claims (versioned: v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestBindingMode {
    /// Claims are not compared with the request (they must still be well-formed).
    Off,
    /// Claims are verified when present; tokens without them are accepted. Default:
    /// receivers ship in this mode before any signer emits the claims.
    #[default]
    Accept,
    /// Tokens without the claims, or with claims that do not match the request, are
    /// rejected.
    Require,
}

impl RequestBindingMode {
    pub const ENV: &'static str = ENV_RECEIVER_MODE;

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "accept" => Some(Self::Accept),
            "require" => Some(Self::Require),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Accept => "accept",
            Self::Require => "require",
        }
    }

    /// From [`ENV_RECEIVER_MODE`]; unset or empty = [`RequestBindingMode::Accept`].
    pub fn from_env() -> Result<Self, BindingConfigError> {
        from_env_value(Self::ENV, "off|accept|require", Self::parse)
    }
}

impl fmt::Display for RequestBindingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a signer emits the binding claims (env [`ENV_SIGNER_EMISSION`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestBindingEmission {
    /// Default for this release: tokens are unbound (as before SEC-012).
    #[default]
    Off,
    /// Tokens signed through `sign_request` carry `bnd` / `htm` / `htu`. Turn on only
    /// after every receiver of this service runs a binding-aware verifier.
    Emit,
}

impl RequestBindingEmission {
    pub const ENV: &'static str = ENV_SIGNER_EMISSION;

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "emit" | "on" => Some(Self::Emit),
            _ => None,
        }
    }

    /// From [`ENV_SIGNER_EMISSION`]; unset or empty = [`RequestBindingEmission::Off`].
    pub fn from_env() -> Result<Self, BindingConfigError> {
        from_env_value(Self::ENV, "off|emit", Self::parse)
    }
}

fn from_env_value<T: Default>(
    variable: &'static str,
    expected: &'static str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T, BindingConfigError> {
    match std::env::var(variable) {
        Ok(v) if !v.trim().is_empty() => parse(&v).ok_or(BindingConfigError { variable, expected }),
        _ => Ok(T::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_paths_are_unchanged() {
        for p in [
            "/",
            "/internal/v1/communications/dispatch",
            "/internal/v1/x/",
            "/internal/v1/a:b@c/~x_y-z.json",
            "/internal/v1/a%2Fb",
            "/internal/v1/%C3%A9",
            "/v1/calls/123;v=2",
        ] {
            assert_eq!(canonical_request_path(p).as_deref(), Ok(p), "{p}");
        }
    }

    #[test]
    fn percent_encoding_is_normalized() {
        assert_eq!(
            canonical_request_path("/internal/v1/%7Euser/%41%62%2d%5F").unwrap(),
            "/internal/v1/~user/Ab-_"
        );
        assert_eq!(
            canonical_request_path("/a%2fb%c3%a9").unwrap(),
            "/a%2Fb%C3%A9"
        );
        // An encoded slash never becomes a separator.
        assert_ne!(
            canonical_request_path("/a%2Fb").unwrap(),
            canonical_request_path("/a/b").unwrap()
        );
    }

    #[test]
    fn trailing_slash_and_case_are_significant() {
        assert_ne!(
            canonical_request_path("/internal/v1/x/").unwrap(),
            canonical_request_path("/internal/v1/x").unwrap()
        );
        assert_ne!(
            canonical_request_path("/internal/v1/X").unwrap(),
            canonical_request_path("/internal/v1/x").unwrap()
        );
    }

    #[test]
    fn non_canonicalizable_paths_are_refused() {
        use RequestPathError::*;
        for (p, e) in [
            ("", NotAbsolute),
            ("internal/v1/x", NotAbsolute),
            ("https://host/internal/v1/x", NotAbsolute),
            ("/internal/v1/x?y=1", QueryOrFragment),
            ("/internal/v1/x#f", QueryOrFragment),
            ("/internal/v1/%", BadEscape),
            ("/internal/v1/%2", BadEscape),
            ("/internal/v1/%zz", BadEscape),
            ("/internal/v1/a b", BadCharacter),
            ("/internal/v1/é", BadCharacter),
            ("/internal/v1/a\\b", BadCharacter),
            ("/internal/v1/../admin", BadSegment),
            ("/internal/v1/./x", BadSegment),
            ("/internal/v1/%2E%2E/admin", BadSegment),
            ("/internal/v1/%2e/x", BadSegment),
            ("/internal/v1//x", BadSegment),
            ("//x", BadSegment),
        ] {
            assert_eq!(canonical_request_path(p), Err(e), "{p:?}");
        }
        assert_eq!(
            canonical_request_path(&format!("/{}", "a".repeat(MAX_REQUEST_PATH_LEN))),
            Err(TooLong)
        );
        assert!(
            canonical_request_path(&format!("/{}", "a".repeat(MAX_REQUEST_PATH_LEN - 1))).is_ok()
        );
    }

    #[test]
    fn methods_are_upper_case_tokens() {
        assert!(is_valid_method("POST"));
        assert!(is_valid_method("PATCH"));
        for bad in ["", "post", "P0ST", "PO ST", &"A".repeat(17)] {
            assert!(!is_valid_method(bad), "{bad:?}");
        }
        assert!(is_mutating_method(&http::Method::POST));
        assert!(is_mutating_method(&http::Method::PUT));
        assert!(is_mutating_method(&http::Method::PATCH));
        assert!(is_mutating_method(&http::Method::DELETE));
        assert!(!is_mutating_method(&http::Method::GET));
        assert!(!is_mutating_method(&http::Method::HEAD));
        assert!(!is_mutating_method(&http::Method::OPTIONS));
    }

    #[test]
    fn claim_values_validate_on_deserialization() {
        assert!(serde_json::from_str::<PathClaim>("\"/internal/v1/x\"").is_ok());
        // Not canonical (lower-case escape, decodable escape, dot segment).
        for bad in ["\"/a%2fb\"", "\"/%41\"", "\"/a/../b\"", "\"/a?b\""] {
            assert!(serde_json::from_str::<PathClaim>(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<MethodClaim>("\"DELETE\"").is_ok());
        assert!(serde_json::from_str::<MethodClaim>("\"delete\"").is_err());
    }

    #[test]
    fn modes_parse() {
        assert_eq!(RequestBindingMode::default(), RequestBindingMode::Accept);
        assert_eq!(
            RequestBindingMode::parse(" Require "),
            Some(RequestBindingMode::Require)
        );
        assert_eq!(
            RequestBindingMode::parse("off"),
            Some(RequestBindingMode::Off)
        );
        assert_eq!(RequestBindingMode::parse("strict"), None);
        assert_eq!(
            RequestBindingEmission::default(),
            RequestBindingEmission::Off
        );
        assert_eq!(
            RequestBindingEmission::parse("emit"),
            Some(RequestBindingEmission::Emit)
        );
        assert_eq!(RequestBindingEmission::parse("yes"), None);
    }
}
