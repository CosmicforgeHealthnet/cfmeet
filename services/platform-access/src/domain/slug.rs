//! Stable, URL-safe keys for tenants, applications and service accounts
//! (`^[a-z0-9][a-z0-9-]{0,62}$`, mirrored by database `CHECK` constraints).

use std::fmt;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Slug(String);

impl Slug {
    pub const MAX_LEN: usize = 63;

    pub fn parse(value: &str) -> Result<Self, String> {
        let bytes = value.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= Self::MAX_LEN
            && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if valid {
            Ok(Self(value.to_string()))
        } else {
            Err(format!(
                "`{}` is not a valid key: use 1-{} lowercase letters, digits or '-', starting with a letter or digit",
                value.chars().take(80).collect::<String>(),
                Self::MAX_LEN
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_stable_keys() {
        for ok in ["a", "acme", "acme-2", "0abc", &"a".repeat(63)] {
            assert!(Slug::parse(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_invalid_keys() {
        for bad in ["", "-a", "Acme", "a_b", "a b", "a.b", &"a".repeat(64)] {
            assert!(Slug::parse(bad).is_err(), "{bad}");
        }
    }
}
