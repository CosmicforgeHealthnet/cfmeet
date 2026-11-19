//! Step 4: the Ed25519 staff assertion. Header exactly `{alg, typ, kid}`; claims closed
//! (unknown members refused); lifetime at most 60 s; roles known; `acr` from the strong
//! set, or the workload acr for the HQ worker's feed identity only.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

use crate::contract::model::{Role, PRODUCT};
use crate::errors::ErrorCode;

pub const MAX_LIFETIME_SECONDS: i64 = 60;
pub const SKEW_SECONDS: i64 = 30;
pub const MAX_APPROVAL_SECONDS: i64 = 43_200;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalClaim {
    pub approval_id: String,
    pub capability: String,
    pub requester_ref: String,
    pub approver_ref: String,
    pub intent_digest: String,
    pub decided_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub product: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub market_scopes: Vec<String>,
    pub organization_scopes: Vec<String>,
    pub zone_scopes: Vec<String>,
    pub tenant_scopes: Vec<String>,
    pub session_id: String,
    pub auth_time: i64,
    pub acr: String,
    pub jti: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub request_digest: String,
    #[serde(default)]
    pub approval: Option<ApprovalClaim>,
}

/// A verified assertion.
#[derive(Debug, Clone)]
pub struct Staff {
    pub claims: Claims,
    pub roles: Vec<Role>,
    pub strong: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

pub fn ref_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-'))
}

pub fn uuid_ok(v: &str) -> bool {
    uuid::Uuid::parse_str(v).is_ok_and(|u| u.hyphenated().to_string() == v)
}

pub fn hex64_ok(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Splits the bearer token and returns the `kid` (unverified) so the key can be looked up.
pub fn kid(token: &str) -> Result<String, ErrorCode> {
    let bad = ErrorCode::StaffAssertionInvalid;
    let header_b64 = token.split('.').next().ok_or(bad)?;
    let raw = B64.decode(header_b64).map_err(|_| bad)?;
    let h: Header = serde_json::from_slice(&raw).map_err(|_| bad)?;
    if h.alg != "EdDSA" || h.typ != "JWT" || h.kid.is_empty() || h.kid.len() > 64 {
        return Err(bad);
    }
    Ok(h.kid)
}

pub struct Expect<'a> {
    pub key: &'a VerifyingKey,
    pub issuer: &'a str,
    pub audience: &'a str,
    pub strong_acr: &'a std::collections::HashSet<String>,
    pub workload_acr: &'a str,
    pub now: DateTime<Utc>,
}

pub fn verify(token: &str, e: &Expect<'_>) -> Result<Staff, ErrorCode> {
    let bad = ErrorCode::StaffAssertionInvalid;
    if token.len() > 8192 {
        return Err(bad);
    }
    let mut parts = token.split('.');
    let (h, c, s) = (
        parts.next().ok_or(bad)?,
        parts.next().ok_or(bad)?,
        parts.next().ok_or(bad)?,
    );
    if parts.next().is_some() {
        return Err(bad);
    }
    kid(token)?;
    let sig: [u8; 64] = B64
        .decode(s)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(bad)?;
    e.key
        .verify(format!("{h}.{c}").as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| bad)?;
    let claims: Claims =
        serde_json::from_slice(&B64.decode(c).map_err(|_| bad)?).map_err(|_| bad)?;

    if claims.iss != e.issuer || claims.aud != e.audience || claims.product != PRODUCT {
        return Err(bad);
    }
    if !ref_ok(&claims.sub)
        || !ref_ok(&claims.session_id)
        || !uuid_ok(&claims.jti)
        || !hex64_ok(&claims.request_digest)
    {
        return Err(bad);
    }
    if claims.exp <= claims.iat
        || claims.exp - claims.iat > MAX_LIFETIME_SECONDS
        || claims.nbf < claims.iat - SKEW_SECONDS
    {
        return Err(bad);
    }
    let now = e.now.timestamp();
    if claims.iat > now + SKEW_SECONDS {
        return Err(bad);
    }
    if now + SKEW_SECONDS < claims.nbf || now >= claims.exp + SKEW_SECONDS {
        return Err(ErrorCode::StaffAssertionExpired);
    }
    for scopes in [
        &claims.market_scopes,
        &claims.organization_scopes,
        &claims.zone_scopes,
        &claims.tenant_scopes,
    ] {
        if scopes.is_empty() || scopes.len() > 64 || !scopes.iter().all(|v| ref_ok(v)) {
            return Err(bad);
        }
    }
    if claims.permissions.is_empty() || claims.permissions.len() > 256 {
        return Err(bad);
    }
    if claims.roles.is_empty() || claims.roles.len() > 16 {
        return Err(bad);
    }
    let roles: Vec<Role> = claims
        .roles
        .iter()
        .map(|r| Role::parse(r))
        .collect::<Option<_>>()
        .ok_or(bad)?;
    let strong = e.strong_acr.contains(&claims.acr);
    let workload = claims.acr == e.workload_acr;
    if workload {
        // The workload acr belongs to the HQ worker alone, and the worker is nothing else.
        if roles != [Role::SourceSync] {
            return Err(bad);
        }
    } else if !strong || roles.contains(&Role::SourceSync) {
        return Err(bad);
    }
    if claims.auth_time <= 0 || claims.auth_time > now + SKEW_SECONDS {
        return Err(bad);
    }
    if let Some(a) = &claims.approval {
        if !uuid_ok(&a.approval_id)
            || !ref_ok(&a.requester_ref)
            || !ref_ok(&a.approver_ref)
            || !hex64_ok(&a.intent_digest)
        {
            return Err(ErrorCode::ApprovalInvalid);
        }
    }
    Ok(Staff {
        claims,
        roles,
        strong,
    })
}

/// Parses an approval timestamp (RFC 3339 UTC, `Z`).
pub fn approval_time(v: &str) -> Option<DateTime<Utc>> {
    v.ends_with('Z')
        .then(|| {
            DateTime::parse_from_rfc3339(v)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        })
        .flatten()
}
