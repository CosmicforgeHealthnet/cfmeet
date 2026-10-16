//! Request, correlation, token and key identifiers.
//!
//! * [`RequestId`] (`req_<32 hex>`) identifies ONE request / service hop and is new for
//!   every hop.
//! * [`CorrelationId`] (`corr_<32 hex>`) is created once per causal operation by the
//!   service where it enters CF Meet and stays stable across every downstream hop.
//!
//! Both are server-generated. Arbitrary client text is never promoted into either.

use std::fmt;

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{value}` is not a valid {kind}")]
pub struct IdError {
    kind: &'static str,
    value: String,
}

fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

macro_rules! prefixed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn generate() -> Self {
                Self(format!("{}{}", $prefix, random_hex()))
            }

            pub fn parse(value: &str) -> Result<Self, IdError> {
                match value.strip_prefix($prefix) {
                    Some(hex) if is_lower_hex(hex, 32) => Ok(Self(value.to_string())),
                    _ => Err(IdError { kind: $kind, value: crate::identity::bounded(value) }),
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }
    };
}

prefixed_id!(
    /// Identifier of one request / one service hop.
    RequestId,
    "req_",
    "request id"
);
prefixed_id!(
    /// Identifier of one causal operation, stable across hops.
    CorrelationId,
    "corr_",
    "correlation id"
);

/// Unique token identifier (`jti`), 32 lower-case hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TokenId(String);

impl TokenId {
    pub fn generate() -> Self {
        Self(random_hex())
    }

    pub fn parse(value: &str) -> Result<Self, IdError> {
        if is_lower_hex(value, 32) {
            Ok(Self(value.to_string()))
        } else {
            Err(IdError {
                kind: "token id",
                value: crate::identity::bounded(value),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TokenId {
    type Error = IdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TokenId> for String {
    fn from(id: TokenId) -> String {
        id.0
    }
}

/// Identifier of a signing key (JWS `kid`): 1–64 of `[A-Za-z0-9._-]`, starting with an
/// alphanumeric character.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyId(String);

impl KeyId {
    pub fn parse(value: &str) -> Result<Self, IdError> {
        let bytes = value.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 64
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if valid {
            Ok(Self(value.to_string()))
        } else {
            Err(IdError {
                kind: "key id",
                value: crate::identity::bounded(value),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for KeyId {
    type Error = IdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<KeyId> for String {
    fn from(id: KeyId) -> String {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_well_formed_and_unique() {
        let (a, b) = (RequestId::generate(), RequestId::generate());
        assert!(a.as_str().starts_with("req_") && a.as_str().len() == 36);
        assert_ne!(a, b);
        assert_eq!(RequestId::parse(a.as_str()).unwrap(), a);
        let c = CorrelationId::generate();
        assert!(c.as_str().starts_with("corr_") && c.as_str().len() == 37);
        assert_eq!(CorrelationId::parse(c.as_str()).unwrap(), c);
        assert_eq!(TokenId::generate().as_str().len(), 32);
    }

    #[test]
    fn client_text_is_never_accepted_as_an_id() {
        for bad in [
            "",
            "req_",
            "my-request",
            "req_ABCDEF0123456789abcdef0123456789",
            "req_0123456789abcdef0123456789abcdef0",
            "corr_0123456789abcdef0123456789abcdef",
            "req_0123456789abcdef0123456789abcde\n",
        ] {
            assert!(RequestId::parse(bad).is_err(), "{bad:?}");
        }
        assert!(CorrelationId::parse("req_0123456789abcdef0123456789abcdef").is_err());
    }

    #[test]
    fn key_ids_are_bounded() {
        assert!(KeyId::parse("calling-staging-2026-10").is_ok());
        for bad in ["", "-x", "a b", "a/b", &"k".repeat(65)] {
            assert!(KeyId::parse(bad).is_err(), "{bad:?}");
        }
    }
}
