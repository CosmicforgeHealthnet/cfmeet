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

/// An HMAC key file (shared with HQ, which reads the same file): base64url text of at
/// least 32 key bytes, padding optional, surrounding whitespace ignored. Returns the key
/// bytes, or `None` for anything else (raw binary files are refused, so both sides always
/// read the same key).
pub fn decode_hmac_key_file(contents: &[u8]) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let text = std::str::from_utf8(contents).ok()?.trim();
    let text = text.trim_end_matches('=');
    if text.is_empty()
        || !text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let key = URL_SAFE_NO_PAD.decode(text).ok()?;
    (key.len() >= 32).then_some(key)
}

#[cfg(test)]
mod key_file_tests {
    use super::decode_hmac_key_file;

    #[test]
    fn hmac_key_files_are_base64url_text_of_at_least_32_bytes() {
        let key = [7u8; 32];
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let text = URL_SAFE_NO_PAD.encode(key);
        assert_eq!(decode_hmac_key_file(text.as_bytes()).unwrap(), key);
        assert_eq!(
            decode_hmac_key_file(
                format!(
                    "{text}
"
                )
                .as_bytes()
            )
            .unwrap(),
            key
        );
        assert_eq!(
            decode_hmac_key_file(format!("{text}=").as_bytes()).unwrap(),
            key
        );
        assert!(
            decode_hmac_key_file(URL_SAFE_NO_PAD.encode([7u8; 31]).as_bytes()).is_none(),
            "too short"
        );
        assert!(decode_hmac_key_file(&key).is_none(), "raw bytes refused");
        assert!(decode_hmac_key_file(b"not base64url!").is_none());
    }
}
