//! Sealing codes, invitation tokens and invitee addresses to the Notification Service
//! (Document 4 §5.1, P4, S-19).
//!
//! HPKE (RFC 9180) base mode with DHKEM(X25519, HKDF-SHA256), HKDF-SHA256 and
//! AES-256-GCM. `info` is fixed ([`HPKE_INFO`]); the AAD binds the sealed part to its
//! envelope (`<producer>|<eventId>|<eventType>`), so a sealed part copied into another
//! event does not open. Producers can only seal: opening is behind the `open` feature,
//! enabled only by the Notification Service renderer. Key rotation uses `kid`
//! (`notif-hpke-YYYY-MM[-suffix]`); the service keeps the previous private key until
//! every event sealed to it has reached a final state.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use hpke::{
    aead::AesGcm256, kdf::HkdfSha256, kem::X25519HkdfSha256, Deserializable, Kem as _, OpModeS,
    Serializable,
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

type Kem = X25519HkdfSha256;

/// HPKE `info` for every sealed part (domain separation, versioned).
pub const HPKE_INFO: &[u8] = b"cfmeet.notifications.sealed.v1";
/// Maximum plaintext size of a sealed part.
pub const MAX_SEALED_PLAINTEXT: usize = 4096;

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SealError {
    #[error("invalid key id (expected notif-hpke-YYYY-MM[-suffix])")]
    InvalidKid,
    #[error("invalid public or private key")]
    InvalidKey,
    #[error("the sealed plaintext is empty, malformed or too large")]
    InvalidPlaintext,
    #[error("the sealed part was sealed to another key id")]
    WrongKid,
    #[error("the sealed part does not open (wrong key, envelope or tampered)")]
    OpenFailed,
}

/// Validates a key id: `notif-hpke-YYYY-MM` with an optional `-[a-z0-9]{1,8}` suffix.
pub fn valid_kid(kid: &str) -> bool {
    let Some(rest) = kid.strip_prefix("notif-hpke-") else {
        return false;
    };
    let b = rest.as_bytes();
    if b.len() < 7 {
        return false;
    }
    let date_ok = b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit);
    if !date_ok {
        return false;
    }
    match &rest[7..] {
        "" => true,
        s => s.strip_prefix('-').is_some_and(|x| {
            (1..=8).contains(&x.len())
                && x.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        }),
    }
}

/// The service's public sealing key, as published to producers (key id + 32-byte X25519
/// public key). Public by nature.
#[derive(Clone)]
pub struct SealingKey {
    kid: String,
    public: <Kem as hpke::Kem>::PublicKey,
}

impl std::fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealingKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl SealingKey {
    pub fn from_bytes(kid: &str, public: &[u8]) -> Result<Self, SealError> {
        if !valid_kid(kid) {
            return Err(SealError::InvalidKid);
        }
        let public =
            <Kem as hpke::Kem>::PublicKey::from_bytes(public).map_err(|_| SealError::InvalidKey)?;
        Ok(Self {
            kid: kid.to_string(),
            public,
        })
    }

    /// `<kid>:<base64url public key>` (the form release tooling distributes).
    pub fn parse(text: &str) -> Result<Self, SealError> {
        let (kid, key) = text.trim().split_once(':').ok_or(SealError::InvalidKey)?;
        let bytes = B64.decode(key).map_err(|_| SealError::InvalidKey)?;
        Self::from_bytes(kid, &bytes)
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn public_bytes(&self) -> Vec<u8> {
        self.public.to_bytes().to_vec()
    }
}

/// What a sealed part contains. Never logged: `Debug` is redacted, memory is zeroized.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct SealedPlaintext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

impl std::fmt::Debug for SealedPlaintext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedPlaintext")
            .field("code", &self.code.as_ref().map(|_| "[REDACTED]"))
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("address", &self.address.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

fn code_ok(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn token_ok(s: &str) -> bool {
    (16..=512).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'~' | b'-'))
}

fn address_ok(s: &str) -> bool {
    match s.split_once('@') {
        Some((local, domain)) => {
            s.len() <= 254
                && (1..=64).contains(&local.len())
                && (1..=253).contains(&domain.len())
                && !domain.contains('@')
                && !s.chars().any(char::is_whitespace)
        }
        None => false,
    }
}

impl SealedPlaintext {
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    /// The fields present (for the catalogue's sealed allowance check).
    pub fn fields(&self) -> Vec<crate::catalogue::SealedField> {
        use crate::catalogue::SealedField::*;
        let mut out = Vec::new();
        if self.code.is_some() {
            out.push(Code);
        }
        if self.token.is_some() {
            out.push(Token);
        }
        if self.address.is_some() {
            out.push(Address);
        }
        out
    }

    /// Same value rules as the contract's `SealedPlaintext`.
    pub fn validate(&self) -> Result<(), SealError> {
        let ok = !self.fields().is_empty()
            && self.code.as_deref().is_none_or(code_ok)
            && self.token.as_deref().is_none_or(token_ok)
            && self.address.as_deref().is_none_or(address_ok);
        if ok {
            Ok(())
        } else {
            Err(SealError::InvalidPlaintext)
        }
    }
}

/// The wire form of a sealed part (`sealed` in the envelope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sealed {
    pub kid: String,
    /// base64url (no padding) of the encapsulated key.
    pub enc: String,
    /// base64url (no padding) of ciphertext || tag.
    pub ct: String,
}

/// The AAD binding a sealed part to its envelope.
pub fn aad(producer: &str, event_id: &str, event_type: &str) -> Vec<u8> {
    format!("{producer}|{event_id}|{event_type}").into_bytes()
}

/// Seals `plaintext` to `key` for the envelope identified by (producer, event id, type).
pub fn seal(
    key: &SealingKey,
    producer: &str,
    event_id: &str,
    event_type: &str,
    plaintext: &SealedPlaintext,
) -> Result<Sealed, SealError> {
    plaintext.validate()?;
    let mut bytes = serde_json::to_vec(plaintext).map_err(|_| SealError::InvalidPlaintext)?;
    if bytes.len() > MAX_SEALED_PLAINTEXT {
        bytes.zeroize();
        return Err(SealError::InvalidPlaintext);
    }
    let result = hpke::single_shot_seal::<AesGcm256, HkdfSha256, Kem>(
        &OpModeS::Base,
        &key.public,
        HPKE_INFO,
        &bytes,
        &aad(producer, event_id, event_type),
    );
    bytes.zeroize();
    let (enc, ct) = result.map_err(|_| SealError::InvalidKey)?;
    Ok(Sealed {
        kid: key.kid.clone(),
        enc: B64.encode(enc.to_bytes()),
        ct: B64.encode(ct),
    })
}

/// Generates a new service key pair: (`<kid>:<base64url public>`, private key bytes).
/// Used by release tooling / tests; the private key is written to a 0400 file and never
/// leaves the service.
pub fn generate_keypair(kid: &str) -> Result<(String, zeroize::Zeroizing<Vec<u8>>), SealError> {
    if !valid_kid(kid) {
        return Err(SealError::InvalidKid);
    }
    let (sk, pk) = Kem::gen_keypair();
    Ok((
        format!("{kid}:{}", B64.encode(pk.to_bytes())),
        zeroize::Zeroizing::new(sk.to_bytes().to_vec()),
    ))
}

#[cfg(feature = "open")]
pub use opening::{open, OpeningKey};

#[cfg(feature = "open")]
mod opening {
    use super::*;
    use hpke::OpModeR;

    /// The service's private key (renderer only). Never logged.
    pub struct OpeningKey {
        kid: String,
        private: <Kem as hpke::Kem>::PrivateKey,
    }

    impl std::fmt::Debug for OpeningKey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("OpeningKey")
                .field("kid", &self.kid)
                .finish_non_exhaustive()
        }
    }

    impl OpeningKey {
        pub fn from_bytes(kid: &str, private: &[u8]) -> Result<Self, SealError> {
            if !valid_kid(kid) {
                return Err(SealError::InvalidKid);
            }
            let private = <Kem as hpke::Kem>::PrivateKey::from_bytes(private)
                .map_err(|_| SealError::InvalidKey)?;
            Ok(Self {
                kid: kid.to_string(),
                private,
            })
        }

        pub fn kid(&self) -> &str {
            &self.kid
        }
    }

    /// Opens a sealed part of the envelope (producer, event id, type). Any change to the
    /// envelope identity, the key or the bytes fails closed.
    pub fn open(
        key: &OpeningKey,
        producer: &str,
        event_id: &str,
        event_type: &str,
        sealed: &Sealed,
    ) -> Result<SealedPlaintext, SealError> {
        if sealed.kid != key.kid {
            return Err(SealError::WrongKid);
        }
        let enc = B64.decode(&sealed.enc).map_err(|_| SealError::OpenFailed)?;
        let ct = B64.decode(&sealed.ct).map_err(|_| SealError::OpenFailed)?;
        let enc =
            <Kem as hpke::Kem>::EncappedKey::from_bytes(&enc).map_err(|_| SealError::OpenFailed)?;
        let mut plain = zeroize::Zeroizing::new(
            hpke::single_shot_open::<AesGcm256, HkdfSha256, Kem>(
                &OpModeR::Base,
                &key.private,
                &enc,
                HPKE_INFO,
                &ct,
                &aad(producer, event_id, event_type),
            )
            .map_err(|_| SealError::OpenFailed)?,
        );
        let value: SealedPlaintext =
            serde_json::from_slice(&plain).map_err(|_| SealError::InvalidPlaintext)?;
        plain.zeroize();
        value.validate()?;
        Ok(value)
    }
}
