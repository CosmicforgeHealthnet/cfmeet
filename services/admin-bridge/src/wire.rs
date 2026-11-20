//! HQ interface v1 wire rules shared by the runtime verifier and the contract fixtures
//! (`docs/contract/fixtures/`), so the published test vectors are produced by the same
//! code that checks real requests.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::contract::model::{HQ_PRINCIPAL, PRODUCT};

pub const SIGNATURE_VERSION: &str = "2";

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// HMAC v2 canonical string: LF-joined `"2"`, principal, METHOD, exact request URI
/// (path and query exactly as sent), unix seconds, nonce, hex sha256 of the exact body.
pub fn hmac_v2_canonical(
    method: &str,
    uri: &str,
    timestamp: i64,
    nonce: &str,
    body: &[u8],
) -> String {
    [
        SIGNATURE_VERSION,
        HQ_PRINCIPAL,
        &method.to_ascii_uppercase(),
        uri,
        &timestamp.to_string(),
        nonce,
        &sha256_hex(body),
    ]
    .join("\n")
}

/// Hex HMAC-SHA256 of the canonical string.
pub fn hmac_v2_signature(
    secret: &[u8],
    method: &str,
    uri: &str,
    timestamp: i64,
    nonce: &str,
    body: &[u8],
) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(hmac_v2_canonical(method, uri, timestamp, nonce, body).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Constant-time check of a received hex signature.
pub fn hmac_v2_verify(
    secret: &[u8],
    method: &str,
    uri: &str,
    timestamp: i64,
    nonce: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    let Ok(received) = hex::decode(signature_hex) else {
        return false;
    };
    if signature_hex.len() != 64 || signature_hex.bytes().any(|b| b.is_ascii_uppercase()) {
        return false;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(hmac_v2_canonical(method, uri, timestamp, nonce, body).as_bytes());
    mac.verify_slice(&received).is_ok()
}

/// `request_digest` claim: hex sha256 of METHOD LF URI LF "cfmeet" LF hex(sha256(body)).
pub fn request_digest(method: &str, uri: &str, body: &[u8]) -> String {
    sha256_hex(
        [
            method.to_ascii_uppercase().as_str(),
            uri,
            PRODUCT,
            &sha256_hex(body),
        ]
        .join("\n")
        .as_bytes(),
    )
}
