use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
/// Exactly 128 random bits, unrelated to account, principal or row identifiers.
pub fn public_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut out = String::from("CF-");
    let mut bits = 0u32;
    let mut n = 0;
    for b in bytes {
        bits = (bits << 8) | u32::from(b);
        n += 8;
        while n >= 5 {
            n -= 5;
            out.push(BASE32[((bits >> n) & 31) as usize] as char);
        }
    }
    if n > 0 {
        out.push(BASE32[((bits << (5 - n)) & 31) as usize] as char);
    }
    out
}
pub fn invite_token() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    hex::encode(b)
}
pub fn protected_digest(key: &[u8], token: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts configured key");
    mac.update(b"cfmeet.communications.invitation.v1\0");
    mac.update(token.as_bytes());
    mac.finalize().into_bytes().to_vec()
}
pub fn digest(value: &serde_json::Value) -> Vec<u8> {
    Sha256::digest(serde_json::to_vec(value).expect("JSON value")).to_vec()
}
pub fn valid_public_id(value: &str) -> bool {
    value.len() == 29 && value.starts_with("CF-") && value[3..].bytes().all(|b| BASE32.contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_use_128_random_bits_and_no_account_data() {
        let ids: std::collections::HashSet<_> = (0..10000).map(|_| public_id()).collect();
        assert_eq!(ids.len(), 10000);
        assert!(ids.iter().all(|s| valid_public_id(s)));
        assert!(!valid_public_id("CF-email@example.com"));
        assert_eq!(invite_token().len(), 64);
    }
    #[test]
    fn invitation_digests_are_keyed_and_domain_separated() {
        let token = invite_token();
        let d = protected_digest(&[1; 32], &token);
        assert_eq!(d.len(), 32);
        assert_ne!(d, protected_digest(&[2; 32], &token));
        assert_ne!(d, Sha256::digest(token.as_bytes()).to_vec());
    }
}
