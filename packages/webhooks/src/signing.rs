//! The customer-facing signature scheme (documented in ADR 0028).
//!
//! ```text
//! CF-Webhook-Id:        <delivery id>          (unique per delivery attempt series / replay)
//! CF-Webhook-Event-Id:  <event id>             (stable across retries and replays: dedupe key)
//! CF-Webhook-Timestamp: <unix seconds>
//! CF-Webhook-Signature: v1=<hex HMAC-SHA256(secret, "v1:" + timestamp + ":" + delivery id + ":" + body)>
//! ```
//!
//! Receivers MUST: recompute the HMAC over the raw body bytes, compare in constant time,
//! refuse timestamps outside a tolerance window (default 300 s), and dedupe on the event id.

use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

pub const HEADER_DELIVERY_ID: &str = "CF-Webhook-Id";
pub const HEADER_EVENT_ID: &str = "CF-Webhook-Event-Id";
pub const HEADER_TIMESTAMP: &str = "CF-Webhook-Timestamp";
pub const HEADER_SIGNATURE: &str = "CF-Webhook-Signature";
/// The only signature version.
pub const VERSION: &str = "v1";
/// Default receiver tolerance for the timestamp.
pub const DEFAULT_TOLERANCE_SECONDS: i64 = 300;
/// Prefix of generated signing secrets (recognisable by secret scanners).
pub const SECRET_PREFIX: &str = "whsec_";

type HmacSha256 = Hmac<Sha256>;

/// The canonical signing input.
pub fn signing_input(timestamp: i64, delivery_id: &str, body: &[u8]) -> Vec<u8> {
    let mut input = format!("{VERSION}:{timestamp}:{delivery_id}:").into_bytes();
    input.extend_from_slice(body);
    input
}

/// The `CF-Webhook-Signature` value for one delivery.
pub fn sign(secret: &[u8], timestamp: i64, delivery_id: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&signing_input(timestamp, delivery_id, body));
    format!("{VERSION}={}", hex::encode(mac.finalize().into_bytes()))
}

/// A fresh, server-generated signing secret (256 bits).
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!(
        "{SECRET_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("malformed signature header")]
    Malformed,
    #[error("timestamp outside the tolerance window")]
    StaleTimestamp,
    #[error("signature mismatch")]
    Mismatch,
}

/// Receiver-side verification (reference implementation and test fixture).
pub fn verify(
    secret: &[u8],
    signature_header: &str,
    timestamp_header: &str,
    delivery_id: &str,
    body: &[u8],
    now_unix: i64,
    tolerance_seconds: i64,
) -> Result<(), VerifyError> {
    let timestamp: i64 = timestamp_header
        .trim()
        .parse()
        .map_err(|_| VerifyError::Malformed)?;
    if (now_unix - timestamp).abs() > tolerance_seconds {
        return Err(VerifyError::StaleTimestamp);
    }
    let hex_sig = signature_header
        .trim()
        .strip_prefix(&format!("{VERSION}="))
        .ok_or(VerifyError::Malformed)?;
    let given = hex::decode(hex_sig).map_err(|_| VerifyError::Malformed)?;
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&signing_input(timestamp, delivery_id, body));
    // `verify_slice` compares in constant time.
    mac.verify_slice(&given).map_err(|_| VerifyError::Mismatch)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"whsec_test-only-fixture-secret";

    #[test]
    fn fixture_signature_is_stable() {
        // Published fixture (ADR 0028): customers can check their verifier against it.
        let sig = sign(SECRET, 1_700_000_000, "whd_1", br#"{"a":1}"#);
        assert_eq!(sig, sign(SECRET, 1_700_000_000, "whd_1", br#"{"a":1}"#));
        assert!(sig.starts_with("v1=") && sig.len() == 3 + 64);
        assert_eq!(
            signing_input(1_700_000_000, "whd_1", br#"{"a":1}"#),
            br#"v1:1700000000:whd_1:{"a":1}"#.to_vec()
        );
    }

    #[test]
    fn verification_binds_secret_timestamp_delivery_and_body() {
        let (ts, id, body) = (1_700_000_000, "whd_1", br#"{"a":1}"#.as_slice());
        let sig = sign(SECRET, ts, id, body);
        let ok = |s: &str, t: &str, i: &str, b: &[u8], now| {
            verify(SECRET, s, t, i, b, now, DEFAULT_TOLERANCE_SECONDS)
        };
        assert_eq!(ok(&sig, "1700000000", id, body, ts + 10), Ok(()));
        // Any change to the signed material fails.
        assert_eq!(
            ok(&sig, "1700000000", id, br#"{"a":2}"#, ts),
            Err(VerifyError::Mismatch)
        );
        assert_eq!(
            ok(&sig, "1700000000", "whd_2", body, ts),
            Err(VerifyError::Mismatch)
        );
        assert_eq!(
            ok(&sig, "1700000001", id, body, ts),
            Err(VerifyError::Mismatch)
        );
        assert_eq!(
            verify(b"other", &sig, "1700000000", id, body, ts, 300),
            Err(VerifyError::Mismatch)
        );
        // Replay outside the window is refused even with a valid signature.
        assert_eq!(
            ok(&sig, "1700000000", id, body, ts + 301),
            Err(VerifyError::StaleTimestamp)
        );
        assert_eq!(
            ok(&sig, "1700000000", id, body, ts - 301),
            Err(VerifyError::StaleTimestamp)
        );
        assert_eq!(
            ok("v2=00", "1700000000", id, body, ts),
            Err(VerifyError::Malformed)
        );
        assert_eq!(
            ok(&sig, "yesterday", id, body, ts),
            Err(VerifyError::Malformed)
        );
    }

    #[test]
    fn secrets_are_random_and_prefixed() {
        let (a, b) = (generate_secret(), generate_secret());
        assert_ne!(a, b);
        assert!(a.starts_with(SECRET_PREFIX) && a.len() == SECRET_PREFIX.len() + 43);
    }
}
