//! Safety-code / QR verification (Part V Appendix G.5, K, L).
//!
//! Two users compare a code derived from each principal's authenticated device key set
//! (the Ed25519 signature public keys of its ACTIVE devices, as the key directory and the
//! key-transparency proofs returned them). If the server added, swapped or hid a device,
//! the two sides compute different codes.
//!
//! * [`principal_fingerprint`] — SHA-256 over a versioned label, the principal id and the
//!   sorted, de-duplicated device keys (independent of device order).
//! * [`safety_code`] — the two fingerprints ordered by principal id, rendered as twelve
//!   groups of five digits (six per principal), so both sides display the same code.
//! * [`qr_payload`] / [`verify_qr`] — the same material as bytes for a QR code scanned by
//!   the other side.
//!
//! The code changes when a device is added or removed (clients surface that as a key
//! change). It deliberately does not include a transparency tree head, which changes on
//! every log append; clients verify KT proofs separately. Pure computation over public
//! keys: no private key is involved, so this is part of the public crate.

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Format version of fingerprints, codes and QR payloads.
pub const VERSION: u8 = 1;
/// Domain separation of every hash here.
pub const LABEL: &[u8] = b"cfmeet-messaging-safety-code:v1";
/// Bytes of a QR payload: version || (principal id, fingerprint) × 2.
pub const QR_PAYLOAD_LEN: usize = 1 + 2 * (16 + 32);
/// Largest device key set accepted (a principal has at most a handful of devices).
pub const MAX_DEVICE_KEYS: usize = 256;

/// One principal's authenticated device key set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalKeys {
    pub principal_id: Uuid,
    /// Ed25519 signature public keys (32 bytes each) of the principal's devices.
    pub device_keys: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationError {
    /// No device key, more than [`MAX_DEVICE_KEYS`], or a key that is not 32 bytes.
    InvalidKeySet,
    /// Both sides name the same principal.
    SamePrincipal,
}

/// SHA-256(LABEL || "principal" || principal id || u32 n || n × (u32 len || key)) over the
/// sorted, de-duplicated keys.
pub fn principal_fingerprint(keys: &PrincipalKeys) -> Result<[u8; 32], VerificationError> {
    let mut sorted: Vec<&[u8]> = keys.device_keys.iter().map(Vec::as_slice).collect();
    if sorted.is_empty() || sorted.len() > MAX_DEVICE_KEYS || sorted.iter().any(|k| k.len() != 32) {
        return Err(VerificationError::InvalidKeySet);
    }
    sorted.sort_unstable();
    sorted.dedup();
    let mut h = Sha256::new();
    h.update(LABEL);
    h.update(b"principal");
    h.update(keys.principal_id.as_bytes());
    h.update((sorted.len() as u32).to_be_bytes());
    for k in sorted {
        h.update((k.len() as u32).to_be_bytes());
        h.update(k);
    }
    Ok(h.finalize().into())
}

/// Both principals' (id, fingerprint), ordered by principal id.
fn ordered(
    a: &PrincipalKeys,
    b: &PrincipalKeys,
) -> Result<[(Uuid, [u8; 32]); 2], VerificationError> {
    if a.principal_id == b.principal_id {
        return Err(VerificationError::SamePrincipal);
    }
    let fa = (a.principal_id, principal_fingerprint(a)?);
    let fb = (b.principal_id, principal_fingerprint(b)?);
    Ok(if fa.0 < fb.0 { [fa, fb] } else { [fb, fa] })
}

/// Six groups of five digits from a fingerprint (each from 5 bytes, mod 100000).
fn digits(fp: &[u8; 32]) -> Vec<String> {
    fp.chunks(5)
        .take(6)
        .map(|c| {
            let n = c.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
            format!("{:05}", n % 100_000)
        })
        .collect()
}

/// The 60-digit safety code both sides display: twelve groups of five digits, the
/// lower principal id's six groups first. Symmetric in `a` and `b`.
pub fn safety_code(a: &PrincipalKeys, b: &PrincipalKeys) -> Result<String, VerificationError> {
    let [lo, hi] = ordered(a, b)?;
    let mut groups = digits(&lo.1);
    groups.extend(digits(&hi.1));
    Ok(groups.join(" "))
}

/// The QR payload: VERSION || lower id || its fingerprint || higher id || its fingerprint.
pub fn qr_payload(a: &PrincipalKeys, b: &PrincipalKeys) -> Result<Vec<u8>, VerificationError> {
    let [lo, hi] = ordered(a, b)?;
    let mut out = Vec::with_capacity(QR_PAYLOAD_LEN);
    out.push(VERSION);
    for (id, fp) in [lo, hi] {
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&fp);
    }
    Ok(out)
}

/// Whether a scanned QR payload matches the key sets this side sees (constant-time
/// comparison of the whole payload). Anything malformed is simply a mismatch.
pub fn verify_qr(scanned: &[u8], mine: &PrincipalKeys, theirs: &PrincipalKeys) -> bool {
    let Ok(expected) = qr_payload(mine, theirs) else {
        return false;
    };
    if scanned.len() != expected.len() {
        return false;
    }
    scanned
        .iter()
        .zip(&expected)
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(id: u128, seeds: &[u8]) -> PrincipalKeys {
        PrincipalKeys {
            principal_id: Uuid::from_u128(id),
            device_keys: seeds.iter().map(|s| vec![*s; 32]).collect(),
        }
    }

    #[test]
    fn the_code_is_symmetric_and_independent_of_device_order() {
        let a = keys(1, &[1, 2]);
        let b = keys(2, &[3]);
        let code = safety_code(&a, &b).unwrap();
        assert_eq!(code, safety_code(&b, &a).unwrap());
        assert_eq!(code, safety_code(&keys(1, &[2, 1, 2]), &b).unwrap());
        assert_eq!(code.split(' ').count(), 12);
        assert!(code
            .split(' ')
            .all(|g| g.len() == 5 && g.bytes().all(|c| c.is_ascii_digit())));
        assert_eq!(qr_payload(&a, &b).unwrap(), qr_payload(&b, &a).unwrap());
        assert_eq!(qr_payload(&a, &b).unwrap().len(), QR_PAYLOAD_LEN);
    }

    #[test]
    fn any_device_change_or_swap_changes_the_code() {
        let a = keys(1, &[1, 2]);
        let b = keys(2, &[3]);
        let code = safety_code(&a, &b).unwrap();
        for (x, y) in [
            (keys(1, &[1, 2, 9]), b.clone()), // a device added (e.g. by the server)
            (keys(1, &[1]), b.clone()),       // a device hidden
            (keys(1, &[1, 4]), b.clone()),    // a device key swapped
            (a.clone(), keys(3, &[3])),       // another principal with the same keys
            (keys(2, &[1, 2]), keys(1, &[3])), // the key sets of the two swapped
        ] {
            assert_ne!(code, safety_code(&x, &y).unwrap());
        }
        let qr = qr_payload(&a, &b).unwrap();
        assert!(verify_qr(&qr, &b, &a));
        assert!(!verify_qr(&qr, &keys(1, &[1, 2, 9]), &b));
        let mut flipped = qr.clone();
        flipped[40] ^= 1;
        assert!(!verify_qr(&flipped, &a, &b));
        assert!(!verify_qr(&qr[..10], &a, &b));
    }

    #[test]
    fn malformed_key_sets_are_refused() {
        let b = keys(2, &[3]);
        assert_eq!(
            safety_code(&keys(1, &[]), &b),
            Err(VerificationError::InvalidKeySet)
        );
        let short = PrincipalKeys {
            principal_id: Uuid::from_u128(1),
            device_keys: vec![vec![1; 31]],
        };
        assert_eq!(
            safety_code(&short, &b),
            Err(VerificationError::InvalidKeySet)
        );
        assert_eq!(safety_code(&b, &b), Err(VerificationError::SamePrincipal));
        assert!(!verify_qr(&[], &b, &b));
    }
}
