use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
/// CF Meet ID alphabet: 32 characters without look-alikes (no I, O, 0 or 1).
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// Characters after the `CF-` prefix (owner decision 2026-11-19: short, easy to share).
pub const PUBLIC_ID_LEN: usize = 6;
/// `CF-` plus 6 random characters (32^6, about 1.07 billion), unrelated to account,
/// principal or row identifiers. Uniqueness is enforced by the table's primary key.
pub fn public_id() -> String {
    let mut out = String::from("CF-");
    let mut b = [0u8; PUBLIC_ID_LEN];
    OsRng.fill_bytes(&mut b);
    for x in b {
        out.push(ALPHABET[(x & 31) as usize] as char);
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
    value.len() == 3 + PUBLIC_ID_LEN
        && value.starts_with("CF-")
        && value[3..].bytes().all(|b| ALPHABET.contains(&b))
}
/// What a person typed, as a CF Meet ID: case-insensitive, surrounding spaces ignored, and
/// the hyphen optional (`cf-ab7k3q`, `CFAB7K3Q`). Anything else stays invalid.
pub fn normalize_public_id(value: &str) -> String {
    let v = value.trim().to_ascii_uppercase();
    match v.strip_prefix("CF") {
        Some(rest) if !rest.starts_with('-') && rest.len() == PUBLIC_ID_LEN => format!("CF-{rest}"),
        _ => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_short_and_avoid_look_alikes() {
        let ids: Vec<_> = (0..10000).map(|_| public_id()).collect();
        assert!(ids.iter().all(|s| valid_public_id(s) && s.len() == 9));
        assert!(ids.iter().all(|s| !s[3..].contains(['I', 'O', '0', '1'])));
        let distinct: std::collections::HashSet<_> = ids.iter().collect();
        assert!(distinct.len() > 9_900, "random enough");
        assert!(!valid_public_id("CF-email@example.com"));
        // Long IDs of the previous format no longer resolve (owner decision).
        assert!(!valid_public_id("CF-ABCDEFGHJKLMNPQRSTUVWXYZ23"));
        assert!(!valid_public_id("CF-ABCDE0"));
        assert_eq!(normalize_public_id(" cf-ab7k3q "), "CF-AB7K3Q");
        assert_eq!(normalize_public_id("cfab7k3q"), "CF-AB7K3Q");
        assert!(valid_public_id(&normalize_public_id("cfab7k3q")));
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
