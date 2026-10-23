//! Canonical content digests used as reconciliation evidence (Section 53 migration:
//! `sha256-canonical-lines-v1`). Not webhook cryptography — signing and sealing come only
//! from `cf_webhooks`.

use sha2::{Digest, Sha256};

/// SHA-256 (hex) over every line followed by `\n`.
pub fn sha256_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let mut h = Sha256::new();
    for l in lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_empty_digest_is_the_sha256_of_nothing() {
        assert_eq!(
            super::sha256_lines([]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_ne!(super::sha256_lines(["a", "b"]), super::sha256_lines(["ab"]));
    }
}
