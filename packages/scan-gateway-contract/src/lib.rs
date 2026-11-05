//! # cf-scan-gateway-contract — CosmicForge shared security scan gateway contract
//!
//! The gateway sits between authorized CosmicForge products and an EXISTING ClamAV engine.
//! This crate holds what both sides must agree on, so neither re-implements it:
//!
//! * the route, headers and limits of `POST /internal/v1/scans`;
//! * the closed verdict vocabulary ([`Verdict`]) and stable reason codes ([`Reason`]);
//! * the [`ScanResponse`] body;
//! * the signed-verdict format ([`signing_message`], [`VerdictKeys::verify`]).
//!
//! Only `CLEAN` and `INFECTED` are definitive and only they are signed. A caller must treat
//! everything else — and any `CLEAN` whose signature, request id, digest or length does not
//! verify — as "no answer" (retry later; never clean). The signature binds the verdict to
//! the authenticated caller, its runtime environment, the request id and the SHA-256 of the
//! exact bytes the engine received, so a verdict can neither be forged by another container
//! on the network nor replayed onto different content.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Contract version carried in every response.
pub const CONTRACT_VERSION: &str = "1";
/// The one scanning operation.
pub const SCAN_PATH: &str = "/internal/v1/scans";
/// Lower-case hex SHA-256 of the body, computed by the caller. Required.
pub const CONTENT_SHA256_HEADER: &str = "x-cf-scan-content-sha256";
/// Media type of the request body: the raw file bytes, never a multipart envelope.
pub const BODY_CONTENT_TYPE: &str = "application/octet-stream";
/// Domain separation tag of the signed verdict.
pub const SIGNING_DOMAIN: &str = "cosmicforge-scan-verdict/v1";
/// Maximum number of simultaneously trusted verdict keys (rotation overlap).
pub const MAX_VERDICT_KEYS: usize = 4;

/// Closed verdict vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    /// The engine inspected every byte and found nothing. Definitive; signed.
    Clean,
    /// The engine found malware. Definitive; signed.
    Infected,
    /// The engine answered with an error. Retry later; never clean.
    Error,
    /// No engine answer (unreachable, overloaded, stale signatures). Retry later.
    Unavailable,
    /// The scan did not finish in time. Retry later.
    Timeout,
    /// The request itself is unacceptable (size, digest, truncation, quota). Not retried
    /// unchanged.
    Rejected,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Clean => "CLEAN",
            Verdict::Infected => "INFECTED",
            Verdict::Error => "ERROR",
            Verdict::Unavailable => "UNAVAILABLE",
            Verdict::Timeout => "TIMEOUT",
            Verdict::Rejected => "REJECTED",
        }
    }

    /// Only these are signed and only these may change an object's state.
    pub fn is_definitive(&self) -> bool {
        matches!(self, Verdict::Clean | Verdict::Infected)
    }

    /// HTTP status the gateway answers with.
    pub fn http_status(&self, reason: Option<Reason>) -> u16 {
        match (self, reason) {
            (Verdict::Clean | Verdict::Infected, _) => 200,
            (Verdict::Rejected, Some(Reason::TooLarge)) => 413,
            (Verdict::Rejected, Some(Reason::RateLimited)) => 429,
            (Verdict::Rejected, _) => 422,
            (Verdict::Error, _) => 502,
            (Verdict::Unavailable | Verdict::Timeout, _) => 503,
        }
    }
}

/// Stable machine-readable reason of a non-definitive verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Reason {
    EmptyContent,
    TooLarge,
    LengthMismatch,
    Truncated,
    DigestMismatch,
    RateLimited,
    Overloaded,
    BackendUnavailable,
    SignaturesStale,
    BackendError,
    MalformedBackendReply,
    ScanTimeout,
}

impl Reason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::EmptyContent => "EMPTY_CONTENT",
            Reason::TooLarge => "TOO_LARGE",
            Reason::LengthMismatch => "LENGTH_MISMATCH",
            Reason::Truncated => "TRUNCATED",
            Reason::DigestMismatch => "DIGEST_MISMATCH",
            Reason::RateLimited => "RATE_LIMITED",
            Reason::Overloaded => "OVERLOADED",
            Reason::BackendUnavailable => "BACKEND_UNAVAILABLE",
            Reason::SignaturesStale => "SIGNATURES_STALE",
            Reason::BackendError => "BACKEND_ERROR",
            Reason::MalformedBackendReply => "MALFORMED_BACKEND_REPLY",
            Reason::ScanTimeout => "SCAN_TIMEOUT",
        }
    }
}

/// Engine facts reported with every verdict that reached the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineFacts {
    /// e.g. `clamav`.
    pub name: String,
    /// e.g. `1.4.6`.
    pub version: String,
    /// The daily signature database version, e.g. `28173`.
    pub signature_version: u32,
    /// Age of the signature database when the scan ran.
    pub signature_age_seconds: u64,
}

/// Body of every answer to an authorized scan request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResponse {
    pub contract_version: String,
    pub request_id: String,
    /// Authenticated caller (from the verified service token, never from the request).
    pub client_id: String,
    pub product: String,
    pub environment: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    /// Signature name of a finding (INFECTED only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<String>,
    /// SHA-256 (lower-case hex) of the bytes the gateway actually received; empty when no
    /// byte was read.
    pub sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<EngineFacts>,
    /// Unix seconds.
    pub scanned_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_key_id: Option<String>,
    /// base64url Ed25519 signature over [`signing_message`] (definitive verdicts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_signature: Option<String>,
}

/// The exact bytes signed for a definitive verdict. Line-separated, no field may contain a
/// newline (enforced by [`sign`] and [`VerdictKeys::verify`]).
pub fn signing_message(r: &ScanResponse) -> Option<String> {
    let engine = r.engine.as_ref()?;
    let finding = r.finding.as_deref().unwrap_or("");
    let fields = [
        SIGNING_DOMAIN,
        &r.contract_version,
        &r.request_id,
        &r.client_id,
        &r.product,
        &r.environment,
        r.verdict.as_str(),
        finding,
        &r.sha256,
        &r.bytes.to_string(),
        &engine.name,
        &engine.version,
        &engine.signature_version.to_string(),
        &r.scanned_at.to_string(),
    ];
    if fields.iter().any(|f| f.contains('\n') || f.contains('\r')) {
        return None;
    }
    Some(fields.join("\n"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerdictError {
    #[error("verdict is not definitive")]
    NotDefinitive,
    #[error("verdict is missing its engine facts or signature")]
    Unsigned,
    #[error("verdict was signed with an untrusted key")]
    UnknownKey,
    #[error("verdict signature is malformed")]
    MalformedSignature,
    #[error("verdict signature does not verify")]
    BadSignature,
    #[error("verdict is bound to another request, caller, environment or content")]
    BindingMismatch,
    #[error("verdict key set is invalid: {0}")]
    InvalidKeySet(String),
}

/// Signs a definitive verdict in place.
pub fn sign(r: &mut ScanResponse, kid: &str, key: &SigningKey) -> Result<(), VerdictError> {
    if !r.verdict.is_definitive() {
        return Err(VerdictError::NotDefinitive);
    }
    r.verdict_key_id = Some(kid.to_string());
    let message = signing_message(r).ok_or(VerdictError::Unsigned)?;
    r.verdict_signature = Some(URL_SAFE_NO_PAD.encode(key.sign(message.as_bytes()).to_bytes()));
    Ok(())
}

/// One published verdict key (`{"kid": "...", "x": "<base64url Ed25519 public key>"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictKey {
    pub kid: String,
    pub x: String,
}

/// What the caller expects the verdict to be bound to.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub request_id: &'a str,
    pub client_id: &'a str,
    pub environment: &'a str,
    pub sha256: &'a str,
    pub bytes: u64,
}

/// Caller-side trusted verdict keys.
#[derive(Debug, Clone)]
pub struct VerdictKeys {
    keys: Vec<(String, VerifyingKey)>,
}

impl VerdictKeys {
    /// Parses `{"keys": [{"kid", "x"}, …]}` (1–4 distinct keys).
    pub fn from_json(json: &str) -> Result<Self, VerdictError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Set {
            keys: Vec<VerdictKey>,
        }
        let set: Set =
            serde_json::from_str(json).map_err(|e| VerdictError::InvalidKeySet(e.to_string()))?;
        Self::new(&set.keys)
    }

    pub fn new(keys: &[VerdictKey]) -> Result<Self, VerdictError> {
        if keys.is_empty() || keys.len() > MAX_VERDICT_KEYS {
            return Err(VerdictError::InvalidKeySet(format!(
                "expected 1–{MAX_VERDICT_KEYS} keys"
            )));
        }
        let mut out: Vec<(String, VerifyingKey)> = Vec::new();
        for k in keys {
            if k.kid.is_empty()
                || k.kid.len() > 64
                || !k
                    .kid
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            {
                return Err(VerdictError::InvalidKeySet("invalid kid".into()));
            }
            if out.iter().any(|(kid, _)| kid == &k.kid) {
                return Err(VerdictError::InvalidKeySet("duplicate kid".into()));
            }
            let raw = URL_SAFE_NO_PAD
                .decode(&k.x)
                .map_err(|_| VerdictError::InvalidKeySet("x is not base64url".into()))?;
            let raw: [u8; 32] = raw
                .try_into()
                .map_err(|_| VerdictError::InvalidKeySet("x is not 32 bytes".into()))?;
            let key = VerifyingKey::from_bytes(&raw)
                .map_err(|_| VerdictError::InvalidKeySet("x is not an Ed25519 key".into()))?;
            out.push((k.kid.clone(), key));
        }
        Ok(Self { keys: out })
    }

    /// Verifies a definitive verdict and its binding. Never returns `Ok` for a verdict
    /// the caller must not act upon.
    pub fn verify(&self, r: &ScanResponse, expected: Expected<'_>) -> Result<(), VerdictError> {
        if !r.verdict.is_definitive() {
            return Err(VerdictError::NotDefinitive);
        }
        if r.contract_version != CONTRACT_VERSION
            || r.request_id != expected.request_id
            || r.client_id != expected.client_id
            || r.environment != expected.environment
            || r.sha256 != expected.sha256
            || r.bytes != expected.bytes
            || (r.verdict == Verdict::Clean && r.finding.is_some())
            || (r.verdict == Verdict::Infected && r.finding.as_deref().unwrap_or("").is_empty())
        {
            return Err(VerdictError::BindingMismatch);
        }
        let kid = r.verdict_key_id.as_deref().ok_or(VerdictError::Unsigned)?;
        let sig = r
            .verdict_signature
            .as_deref()
            .ok_or(VerdictError::Unsigned)?;
        let key = self
            .keys
            .iter()
            .find(|(k, _)| k == kid)
            .map(|(_, key)| key)
            .ok_or(VerdictError::UnknownKey)?;
        let sig: [u8; 64] = URL_SAFE_NO_PAD
            .decode(sig)
            .map_err(|_| VerdictError::MalformedSignature)?
            .try_into()
            .map_err(|_| VerdictError::MalformedSignature)?;
        let message = signing_message(r).ok_or(VerdictError::Unsigned)?;
        key.verify(message.as_bytes(), &Signature::from_bytes(&sig))
            .map_err(|_| VerdictError::BadSignature)
    }
}

/// The public half of a verdict signing key, for publishing to callers.
pub fn public_key(kid: &str, key: &SigningKey) -> VerdictKey {
    VerdictKey {
        kid: kid.to_string(),
        x: URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
    }
}

/// Lower-case 64-hex check.
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::generate(&mut rand_core::OsRng)
    }

    fn clean() -> ScanResponse {
        ScanResponse {
            contract_version: CONTRACT_VERSION.into(),
            request_id: "req_1".into(),
            client_id: "cfmeet.messaging".into(),
            product: "cfmeet".into(),
            environment: "production".into(),
            verdict: Verdict::Clean,
            reason: None,
            finding: None,
            sha256: "a".repeat(64),
            bytes: 12,
            engine: Some(EngineFacts {
                name: "clamav".into(),
                version: "1.4.6".into(),
                signature_version: 28173,
                signature_age_seconds: 10,
            }),
            scanned_at: 1_800_000_000,
            verdict_key_id: None,
            verdict_signature: None,
        }
    }

    fn expected<'a>(r: &'a ScanResponse) -> Expected<'a> {
        Expected {
            request_id: &r.request_id,
            client_id: &r.client_id,
            environment: &r.environment,
            sha256: &r.sha256,
            bytes: r.bytes,
        }
    }

    #[test]
    fn a_signed_verdict_verifies_only_for_its_exact_binding() {
        let k = key();
        let keys = VerdictKeys::new(&[public_key("v1", &k)]).unwrap();
        let mut r = clean();
        sign(&mut r, "v1", &k).unwrap();
        keys.verify(&r, expected(&r)).unwrap();

        let other = "b".repeat(64);
        let e = Expected {
            sha256: &other,
            ..expected(&r)
        };
        assert_eq!(keys.verify(&r, e), Err(VerdictError::BindingMismatch));
        let e = Expected {
            request_id: "req_2",
            ..expected(&r)
        };
        assert_eq!(keys.verify(&r, e), Err(VerdictError::BindingMismatch));
        let e = Expected {
            client_id: "cfmeet.calling",
            ..expected(&r)
        };
        assert_eq!(keys.verify(&r, e), Err(VerdictError::BindingMismatch));
        let e = Expected {
            environment: "staging",
            ..expected(&r)
        };
        assert_eq!(keys.verify(&r, e), Err(VerdictError::BindingMismatch));
        let e = Expected {
            bytes: 13,
            ..expected(&r)
        };
        assert_eq!(keys.verify(&r, e), Err(VerdictError::BindingMismatch));
    }

    #[test]
    fn tampering_with_any_signed_field_is_detected() {
        let k = key();
        let keys = VerdictKeys::new(&[public_key("v1", &k)]).unwrap();
        let mut r = clean();
        r.verdict = Verdict::Infected;
        r.finding = Some("Eicar-Test-Signature".into());
        sign(&mut r, "v1", &k).unwrap();
        // An attacker flipping INFECTED to CLEAN.
        let mut forged = r.clone();
        forged.verdict = Verdict::Clean;
        forged.finding = None;
        assert_eq!(
            keys.verify(&forged, expected(&forged)),
            Err(VerdictError::BadSignature)
        );
        let mut forged = r.clone();
        forged.engine.as_mut().unwrap().signature_version = 1;
        assert_eq!(
            keys.verify(&forged, expected(&forged)),
            Err(VerdictError::BadSignature)
        );
        let mut forged = r.clone();
        forged.scanned_at += 1;
        assert_eq!(
            keys.verify(&forged, expected(&forged)),
            Err(VerdictError::BadSignature)
        );
    }

    #[test]
    fn unsigned_foreign_and_non_definitive_verdicts_never_verify() {
        let k = key();
        let keys = VerdictKeys::new(&[public_key("v1", &k)]).unwrap();
        let r = clean();
        assert_eq!(keys.verify(&r, expected(&r)), Err(VerdictError::Unsigned));
        let mut r = clean();
        sign(&mut r, "v1", &key()).unwrap();
        assert_eq!(
            keys.verify(&r, expected(&r)),
            Err(VerdictError::BadSignature)
        );
        let mut r = clean();
        sign(&mut r, "v9", &k).unwrap();
        assert_eq!(keys.verify(&r, expected(&r)), Err(VerdictError::UnknownKey));
        let mut r = clean();
        r.verdict = Verdict::Unavailable;
        assert_eq!(sign(&mut r, "v1", &k), Err(VerdictError::NotDefinitive));
        assert_eq!(
            keys.verify(&r, expected(&r)),
            Err(VerdictError::NotDefinitive)
        );
        // A CLEAN carrying a finding, or an INFECTED without one, is malformed.
        let mut r = clean();
        r.finding = Some("x".into());
        sign(&mut r, "v1", &k).unwrap();
        assert_eq!(
            keys.verify(&r, expected(&r)),
            Err(VerdictError::BindingMismatch)
        );
    }

    #[test]
    fn newline_injection_cannot_create_an_ambiguous_message() {
        let k = key();
        let mut r = clean();
        r.request_id = "req_1\nINFECTED".into();
        assert_eq!(sign(&mut r, "v1", &k), Err(VerdictError::Unsigned));
    }

    #[test]
    fn key_sets_are_bounded_and_validated() {
        let k = key();
        assert!(VerdictKeys::from_json(r#"{"keys":[]}"#).is_err());
        assert!(VerdictKeys::from_json(r#"{"keys":[{"kid":"a","x":"zz"}]}"#).is_err());
        let p = public_key("a", &k);
        assert!(VerdictKeys::new(&[p.clone(), p.clone()]).is_err());
        let five: Vec<_> = (0..5)
            .map(|i| public_key(&format!("k{i}"), &key()))
            .collect();
        assert!(VerdictKeys::new(&five).is_err());
        let json = serde_json::json!({"keys": [p]}).to_string();
        VerdictKeys::from_json(&json).unwrap();
        assert!(VerdictKeys::from_json(r#"{"keys":[],"extra":1}"#).is_err());
    }

    #[test]
    fn wire_format_is_stable() {
        let r = clean();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["verdict"], "CLEAN");
        assert_eq!(v["engine"]["signatureVersion"], 28173);
        assert!(v.get("reason").is_none());
        assert_eq!(
            serde_json::to_value(Reason::SignaturesStale).unwrap(),
            "SIGNATURES_STALE"
        );
        assert_eq!(Verdict::Rejected.http_status(Some(Reason::TooLarge)), 413);
        assert_eq!(Verdict::Timeout.http_status(None), 503);
        assert!(is_sha256_hex(&"0".repeat(64)) && !is_sha256_hex(&"A".repeat(64)));
    }
}
