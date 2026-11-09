//! Service token wire format (contracts `cfmeet.platform.service-token-header` and
//! `cfmeet.platform.service-token-claims`, v1).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    binding::{MethodClaim, PathClaim, RequestBinding, REQUEST_BINDING_VERSION},
    delegated::DelegatedPlatformContext,
    identity::{RuntimeEnvironment, ServiceId},
    ids::{CorrelationId, KeyId, RequestId, TokenId},
};

/// The only accepted JWS algorithm (Ed25519).
pub const ALGORITHM: &str = "EdDSA";
/// Explicit token type (RFC 8725 §3.11): prevents confusion with any other JWT.
pub const TOKEN_TYPE: &str = "cf-service+jwt";
/// Claims contract major version.
pub const CLAIMS_VERSION: u8 = 1;
/// Default token lifetime.
pub const DEFAULT_TTL_SECONDS: u64 = 120;
/// Hard maximum token lifetime accepted by any verifier and producible by any signer.
pub const MAX_TTL_SECONDS: u64 = 300;
/// Minimum configurable lifetime.
pub const MIN_TTL_SECONDS: u64 = 10;
/// Maximum tolerated clock skew between services.
pub const MAX_CLOCK_SKEW_SECONDS: u64 = 30;
/// Presented tokens longer than this are rejected before any parsing.
pub const MAX_TOKEN_LEN: usize = 4096;

/// Claims of a service token. Produced only by the signer; the claim set is closed
/// (unknown claims are rejected), so a token can never carry self-declared permissions.
///
/// The optional request binding group (`bnd`, `htm`, `htu`; [`crate::binding`]) is
/// all-or-nothing: a partial group or an unknown `bnd` is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawClaims")]
pub struct ServiceTokenClaims {
    pub(crate) ver: u8,
    pub(crate) iss: ServiceId,
    pub(crate) aud: ServiceId,
    pub(crate) runtime_environment: RuntimeEnvironment,
    pub(crate) iat: i64,
    pub(crate) nbf: i64,
    pub(crate) exp: i64,
    pub(crate) jti: TokenId,
    pub(crate) request_id: RequestId,
    pub(crate) correlation_id: CorrelationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delegated_context: Option<DelegatedPlatformContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bnd: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) htm: Option<MethodClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) htu: Option<PathClaim>,
}

/// Wire shape (closed claim set); [`ServiceTokenClaims`] adds the binding group rule.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClaims {
    ver: u8,
    iss: ServiceId,
    aud: ServiceId,
    runtime_environment: RuntimeEnvironment,
    iat: i64,
    nbf: i64,
    exp: i64,
    jti: TokenId,
    request_id: RequestId,
    correlation_id: CorrelationId,
    #[serde(default)]
    delegated_context: Option<DelegatedPlatformContext>,
    #[serde(default)]
    bnd: Option<u8>,
    #[serde(default)]
    htm: Option<MethodClaim>,
    #[serde(default)]
    htu: Option<PathClaim>,
}

impl TryFrom<RawClaims> for ServiceTokenClaims {
    type Error = &'static str;

    fn try_from(r: RawClaims) -> Result<Self, Self::Error> {
        match (r.bnd, &r.htm, &r.htu) {
            (None, None, None) => {}
            (Some(REQUEST_BINDING_VERSION), Some(_), Some(_)) => {}
            (Some(_), Some(_), Some(_)) => return Err("unsupported request binding version"),
            _ => return Err("bnd, htm and htu must be present together"),
        }
        Ok(Self {
            ver: r.ver,
            iss: r.iss,
            aud: r.aud,
            runtime_environment: r.runtime_environment,
            iat: r.iat,
            nbf: r.nbf,
            exp: r.exp,
            jti: r.jti,
            request_id: r.request_id,
            correlation_id: r.correlation_id,
            delegated_context: r.delegated_context,
            bnd: r.bnd,
            htm: r.htm,
            htu: r.htu,
        })
    }
}

impl ServiceTokenClaims {
    /// Parses claims JSON (contract conformance and diagnostics). Parsing does not verify
    /// anything; only [`crate::ServiceTokenVerifier`] establishes trust.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn issuer(&self) -> &ServiceId {
        &self.iss
    }

    pub fn audience(&self) -> &ServiceId {
        &self.aud
    }

    pub fn runtime_environment(&self) -> RuntimeEnvironment {
        self.runtime_environment
    }

    pub fn issued_at(&self) -> i64 {
        self.iat
    }

    pub fn not_before(&self) -> i64 {
        self.nbf
    }

    pub fn expires_at(&self) -> i64 {
        self.exp
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

    /// The request binding (`bnd` / `htm` / `htu`), when the token carries one.
    pub fn request_binding(&self) -> Option<RequestBinding> {
        match (&self.htm, &self.htu) {
            (Some(method), Some(path)) => Some(RequestBinding {
                method: method.clone(),
                path: path.clone(),
            }),
            _ => None,
        }
    }
}

/// Strictly parsed protected header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtectedHeader {
    pub kid: KeyId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderProblem {
    Malformed,
    Algorithm,
    TokenType,
    ForbiddenParameter,
}

/// Parses the first JWS segment: exactly `{alg, typ, kid}`, `alg` = EdDSA, `typ` =
/// cf-service+jwt. Any other parameter (`jku`, `jwk`, `x5u`, `x5c`, `crit`, …) is refused
/// so a token can never select its own key, algorithm or processing rules.
pub(crate) fn parse_protected_header(segment: &str) -> Result<ProtectedHeader, HeaderProblem> {
    parse_protected_header_of(segment, TOKEN_TYPE)
}

/// [`parse_protected_header`] for a token of explicit type `typ` (each token kind has its
/// own `typ`, so a token of one kind is never accepted as another).
pub(crate) fn parse_protected_header_of(
    segment: &str,
    typ: &str,
) -> Result<ProtectedHeader, HeaderProblem> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| HeaderProblem::Malformed)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| HeaderProblem::Malformed)?;
    let object = value.as_object().ok_or(HeaderProblem::Malformed)?;
    if object
        .keys()
        .any(|k| !matches!(k.as_str(), "alg" | "typ" | "kid"))
    {
        return Err(HeaderProblem::ForbiddenParameter);
    }
    match object.get("alg").and_then(Value::as_str) {
        Some(ALGORITHM) => {}
        _ => return Err(HeaderProblem::Algorithm),
    }
    match object.get("typ").and_then(Value::as_str) {
        Some(t) if t == typ => {}
        _ => return Err(HeaderProblem::TokenType),
    }
    let kid = object
        .get("kid")
        .and_then(Value::as_str)
        .and_then(|k| KeyId::parse(k).ok())
        .ok_or(HeaderProblem::Malformed)?;
    Ok(ProtectedHeader { kid })
}

/// Syntactic shape of a compact JWS: three non-empty base64url segments.
pub(crate) fn split_compact(token: &str) -> Option<(&str, &str, &str)> {
    if token.len() > MAX_TOKEN_LEN
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    let mut parts = token.split('.');
    let (h, p, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || h.is_empty() || p.is_empty() || s.is_empty() {
        return None;
    }
    Some((h, p, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(v: Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap())
    }

    #[test]
    fn header_must_be_exactly_alg_typ_kid() {
        let ok = b64(serde_json::json!({"alg":"EdDSA","typ":"cf-service+jwt","kid":"k1"}));
        assert_eq!(parse_protected_header(&ok).unwrap().kid.as_str(), "k1");
        for (header, problem) in [
            (
                serde_json::json!({"alg":"none","typ":"cf-service+jwt","kid":"k1"}),
                HeaderProblem::Algorithm,
            ),
            (
                serde_json::json!({"alg":"HS256","typ":"cf-service+jwt","kid":"k1"}),
                HeaderProblem::Algorithm,
            ),
            (
                serde_json::json!({"alg":"RS256","typ":"cf-service+jwt","kid":"k1"}),
                HeaderProblem::Algorithm,
            ),
            (
                serde_json::json!({"alg":"EdDSA","typ":"JWT","kid":"k1"}),
                HeaderProblem::TokenType,
            ),
            (
                serde_json::json!({"alg":"EdDSA","kid":"k1"}),
                HeaderProblem::TokenType,
            ),
            (
                serde_json::json!({"alg":"EdDSA","typ":"cf-service+jwt"}),
                HeaderProblem::Malformed,
            ),
            (
                serde_json::json!({"alg":"EdDSA","typ":"cf-service+jwt","kid":"k1","jku":"https://x"}),
                HeaderProblem::ForbiddenParameter,
            ),
            (
                serde_json::json!({"alg":"EdDSA","typ":"cf-service+jwt","kid":"k1","jwk":{}}),
                HeaderProblem::ForbiddenParameter,
            ),
            (
                serde_json::json!({"alg":"EdDSA","typ":"cf-service+jwt","kid":"k1","crit":["exp"]}),
                HeaderProblem::ForbiddenParameter,
            ),
        ] {
            assert_eq!(
                parse_protected_header(&b64(header.clone())),
                Err(problem),
                "{header}"
            );
        }
        assert_eq!(parse_protected_header("!!"), Err(HeaderProblem::Malformed));
    }

    #[test]
    fn compact_serialization_shape() {
        assert!(split_compact("a.b.c").is_some());
        for bad in [
            "",
            "a.b",
            "a.b.c.d",
            "a..c",
            "a.b.",
            "a b.c.d",
            &"a".repeat(MAX_TOKEN_LEN + 1),
        ] {
            assert!(split_compact(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn lifetime_limits_are_coherent() {
        const { assert!(DEFAULT_TTL_SECONDS <= MAX_TTL_SECONDS) };
        const { assert!(MIN_TTL_SECONDS < DEFAULT_TTL_SECONDS) };
        const { assert!(MAX_CLOCK_SKEW_SECONDS <= 30) };
    }
}
