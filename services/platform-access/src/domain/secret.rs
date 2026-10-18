//! Redacted container for a raw API credential.
//!
//! [`IssuedSecret`] holds the full credential string between issuance and its single
//! display to the administrator who created it. It deliberately implements neither
//! `Display`, `Serialize` nor `Clone`; its `Debug` output is `[REDACTED]`; and its memory
//! is zeroized on drop (`secrecy`).

use std::fmt;

use secrecy::{ExposeSecret, SecretString};

pub struct IssuedSecret(SecretString);

impl IssuedSecret {
    pub(crate) fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }

    /// The raw credential, for the one-time display at creation / rotation. The value is
    /// never persisted and cannot be recovered afterwards.
    pub fn expose_once(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for IssuedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IssuedSecret([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_is_redacted() {
        let s = IssuedSecret::new("cfmk_live_supersecretvalue".into());
        let dbg = format!("{s:?} {s:#?}");
        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("supersecret"));
        assert_eq!(s.expose_once(), "cfmk_live_supersecretvalue");
    }
}
