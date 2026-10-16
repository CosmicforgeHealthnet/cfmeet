//! Ed25519 key material.
//!
//! Private keys are loaded from PKCS#8 PEM (`openssl genpkey -algorithm ed25519`, or
//! `cf-service-keygen generate`), never have a default or fallback, are zeroized on drop
//! and never appear in `Debug`, errors or logs. Public keys are distributed as OKP JWKs
//! (RFC 8037) in receivers' trust bundles.

use std::{fmt, path::Path};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ed25519_dalek::{
    pkcs8::{DecodePrivateKey, EncodePrivateKey},
    SigningKey, VerifyingKey,
};
use jsonwebtoken::{DecodingKey, EncodingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::ids::KeyId;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// The message never contains key material.
    #[error("private key file {path} cannot be read")]
    Unreadable { path: String },
    #[error("private key is not an Ed25519 PKCS#8 PEM key")]
    InvalidPrivateKey,
    #[error("public key {kid} is not a valid Ed25519 key")]
    InvalidPublicKey { kid: String },
    #[error("public key {kid} is a weak (small-order) Ed25519 key")]
    WeakPublicKey { kid: String },
}

/// The service's own signing key. `Debug` is redacted.
pub struct ServiceSigningKey {
    kid: KeyId,
    signing: SigningKey,
    encoding: EncodingKey,
}

impl ServiceSigningKey {
    fn from_signing_key(kid: KeyId, signing: SigningKey) -> Result<Self, KeyError> {
        let der = signing
            .to_pkcs8_der()
            .map_err(|_| KeyError::InvalidPrivateKey)?;
        let encoding = EncodingKey::from_ed_der(der.as_bytes());
        Ok(Self {
            kid,
            signing,
            encoding,
        })
    }

    /// A fresh key from the OS CSPRNG (key generation tooling and tests).
    pub fn generate(kid: KeyId) -> Self {
        Self::from_signing_key(kid, SigningKey::generate(&mut OsRng))
            .expect("a freshly generated key encodes")
    }

    pub fn from_pkcs8_pem(kid: KeyId, pem: &str) -> Result<Self, KeyError> {
        let signing = SigningKey::from_pkcs8_pem(pem).map_err(|_| KeyError::InvalidPrivateKey)?;
        Self::from_signing_key(kid, signing)
    }

    /// Loads the key from a secret file. Any failure is fatal (no fallback key).
    pub fn from_pkcs8_pem_file(kid: KeyId, path: &Path) -> Result<Self, KeyError> {
        let pem =
            Zeroizing::new(
                std::fs::read_to_string(path).map_err(|_| KeyError::Unreadable {
                    path: path.display().to_string(),
                })?,
            );
        Self::from_pkcs8_pem(kid, &pem)
    }

    /// PKCS#8 PEM of the private key — for writing a newly generated key to its secret
    /// file only.
    pub fn to_pkcs8_pem(&self) -> Zeroizing<String> {
        self.signing
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .map(|pem| Zeroizing::new(pem.to_string()))
            .expect("an Ed25519 key always encodes")
    }

    pub fn kid(&self) -> &KeyId {
        &self.kid
    }

    pub fn public_jwk(&self) -> PublicJwk {
        PublicJwk {
            kid: self.kid.clone(),
            kty: "OKP".to_string(),
            crv: "Ed25519".to_string(),
            x: URL_SAFE_NO_PAD.encode(self.signing.verifying_key().to_bytes()),
        }
    }

    pub(crate) fn encoding_key(&self) -> &EncodingKey {
        &self.encoding
    }
}

impl fmt::Debug for ServiceSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceSigningKey")
            .field("kid", &self.kid)
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

/// Ed25519 public key as an OKP JWK (contract `trust-bundle` `PublicKey`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicJwk {
    pub kid: KeyId,
    pub kty: String,
    pub crv: String,
    pub x: String,
}

/// Validates a 32-byte Ed25519 public key (point decompression, not weak) and converts it
/// for the JWS verifier.
pub(crate) fn decoding_key(kid: &str, x: &str) -> Result<DecodingKey, KeyError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(x)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .ok_or_else(|| KeyError::InvalidPublicKey {
            kid: kid.to_string(),
        })?;
    let verifying = VerifyingKey::from_bytes(&bytes).map_err(|_| KeyError::InvalidPublicKey {
        kid: kid.to_string(),
    })?;
    if verifying.is_weak() {
        return Err(KeyError::WeakPublicKey {
            kid: kid.to_string(),
        });
    }
    Ok(DecodingKey::from_ed_der(&verifying.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kid() -> KeyId {
        KeyId::parse("test-key-1").unwrap()
    }

    #[test]
    fn pem_round_trip_preserves_the_public_key() {
        let key = ServiceSigningKey::generate(kid());
        let pem = key.to_pkcs8_pem();
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        let loaded = ServiceSigningKey::from_pkcs8_pem(kid(), &pem).unwrap();
        assert_eq!(loaded.public_jwk(), key.public_jwk());
        assert!(decoding_key("k", &key.public_jwk().x).is_ok());
    }

    #[test]
    fn debug_and_errors_never_contain_key_material() {
        let key = ServiceSigningKey::generate(kid());
        let pem = key.to_pkcs8_pem();
        let body: String = pem.lines().nth(1).unwrap().to_string();
        assert!(!format!("{key:?}").contains(&body));
        assert!(format!("{key:?}").contains("[REDACTED]"));
        let broken = pem.replace(&body[..10], "AAAAAAAAAA");
        let err = ServiceSigningKey::from_pkcs8_pem(kid(), &broken).unwrap_err();
        assert!(!err.to_string().contains(&body[10..20]));
    }

    #[test]
    fn invalid_public_keys_are_rejected() {
        assert!(decoding_key("k", "AAAA").is_err());
        assert!(decoding_key("k", "not base64 !").is_err());
        // The identity point is a small-order (weak) key.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(matches!(
            decoding_key("k", &URL_SAFE_NO_PAD.encode(identity)),
            Err(KeyError::WeakPublicKey { .. })
        ));
    }

    #[test]
    fn missing_key_file_fails_closed() {
        let err = ServiceSigningKey::from_pkcs8_pem_file(kid(), Path::new("/nonexistent/key.pem"))
            .unwrap_err();
        assert!(matches!(err, KeyError::Unreadable { .. }));
    }
}
