//! The JWS backend of every CF Meet token signed or verified with `jsonwebtoken`.
//!
//! CF Meet uses exactly two JWS algorithms: **EdDSA** (Ed25519 — service tokens, number
//! grants, contact delegations) and **HMAC-SHA2** (`HS256` — Calling's signaling and media
//! credentials). `jsonwebtoken`'s `rust_crypto` feature links every algorithm it supports
//! (RSA, P-256, P-384), whose elliptic-curve stack is the subject of RUSTSEC-2026-0387
//! (`crypto-bigint 0.5`). This module therefore installs a provider with ONLY the
//! algorithms CF Meet uses, built on the same RustCrypto crates `rust_crypto` uses
//! (`ed25519-dalek`, `hmac`, `sha2`): signatures are byte-identical (both algorithms are
//! deterministic; `tests/jws_provider.rs` pins golden tokens produced by the former
//! backend). Every other algorithm is refused (`InvalidAlgorithm`), as the verifiers'
//! algorithm pinning already did.
//!
//! The signer / verifier implementations follow `jsonwebtoken`'s own RustCrypto backend
//! (MIT, Copyright (c) 2015 Vincent Prouillet).
//!
//! Use [`encode`], [`decode`] and [`decode_header`] (or call [`install`] first): they
//! install the provider before `jsonwebtoken` looks for one, so no code path can reach its
//! "no provider" panic.

use std::sync::Once;

use ed25519_dalek::pkcs8::DecodePrivateKey;
use hmac::{digest::KeyInit, Hmac, Mac};
use jsonwebtoken::{
    crypto::{CryptoProvider, JwtSigner, JwtVerifier, KeyUtils},
    errors::{new_error, Error, ErrorKind, Result},
    jwk::{EllipticCurve, ThumbprintHash},
    signature::{self, Signer, Verifier},
    Algorithm, AlgorithmFamily, DecodingKey, EncodingKey, Header, TokenData, Validation,
};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256, Sha384, Sha512};

// ---------------------------------------------------------------------------------------
// EdDSA (Ed25519)
// ---------------------------------------------------------------------------------------

struct EdDsaSigner(ed25519_dalek::SigningKey);

impl Signer<Vec<u8>> for EdDsaSigner {
    fn try_sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, signature::Error> {
        Ok(ed25519_dalek::Signer::sign(&self.0, msg)
            .to_bytes()
            .to_vec())
    }
}

impl JwtSigner for EdDsaSigner {
    fn algorithm(&self) -> Algorithm {
        Algorithm::EdDSA
    }
}

struct EdDsaVerifier(ed25519_dalek::VerifyingKey);

impl Verifier<Vec<u8>> for EdDsaVerifier {
    fn verify(&self, msg: &[u8], sig: &Vec<u8>) -> std::result::Result<(), signature::Error> {
        let sig = ed25519_dalek::Signature::from_slice(sig)?;
        ed25519_dalek::Verifier::verify(&self.0, msg, &sig)
    }
}

impl JwtVerifier for EdDsaVerifier {
    fn algorithm(&self) -> Algorithm {
        Algorithm::EdDSA
    }
}

fn eddsa_signer(key: &EncodingKey) -> Result<EdDsaSigner> {
    if key.family() != AlgorithmFamily::Ed {
        return Err(new_error(ErrorKind::InvalidKeyFormat));
    }
    Ok(EdDsaSigner(
        ed25519_dalek::SigningKey::from_pkcs8_der(key.as_bytes())
            .map_err(|_| ErrorKind::InvalidEddsaKey)?,
    ))
}

fn eddsa_verifier(key: &DecodingKey) -> Result<EdDsaVerifier> {
    if key.family() != AlgorithmFamily::Ed {
        return Err(new_error(ErrorKind::InvalidKeyFormat));
    }
    let bytes = key.try_get_as_bytes()?;
    let raw: &[u8; 32] = bytes
        .get(..32)
        .and_then(|b| b.try_into().ok())
        .ok_or(ErrorKind::InvalidEddsaKey)?;
    Ok(EdDsaVerifier(
        ed25519_dalek::VerifyingKey::from_bytes(raw).map_err(|_| ErrorKind::InvalidEddsaKey)?,
    ))
}

// ---------------------------------------------------------------------------------------
// HMAC-SHA2
// ---------------------------------------------------------------------------------------

macro_rules! hmac_algorithm {
    ($signer:ident, $verifier:ident, $alg:expr, $hash:ty) => {
        struct $signer(Hmac<$hash>);

        impl Signer<Vec<u8>> for $signer {
            fn try_sign(&self, msg: &[u8]) -> std::result::Result<Vec<u8>, signature::Error> {
                let mut mac = self.0.clone();
                mac.update(msg);
                Ok(mac.finalize().into_bytes().to_vec())
            }
        }

        impl JwtSigner for $signer {
            fn algorithm(&self) -> Algorithm {
                $alg
            }
        }

        struct $verifier(Hmac<$hash>);

        impl Verifier<Vec<u8>> for $verifier {
            fn verify(
                &self,
                msg: &[u8],
                sig: &Vec<u8>,
            ) -> std::result::Result<(), signature::Error> {
                let mut mac = self.0.clone();
                mac.update(msg);
                // Constant-time comparison.
                mac.verify_slice(sig).map_err(signature::Error::from_source)
            }
        }

        impl JwtVerifier for $verifier {
            fn algorithm(&self) -> Algorithm {
                $alg
            }
        }
    };
}

hmac_algorithm!(Hs256Signer, Hs256Verifier, Algorithm::HS256, Sha256);
hmac_algorithm!(Hs384Signer, Hs384Verifier, Algorithm::HS384, Sha384);
hmac_algorithm!(Hs512Signer, Hs512Verifier, Algorithm::HS512, Sha512);

fn hmac_key<M: KeyInit>(family: AlgorithmFamily, bytes: Result<&[u8]>) -> Result<M> {
    if family != AlgorithmFamily::Hmac {
        return Err(new_error(ErrorKind::InvalidKeyFormat));
    }
    M::new_from_slice(bytes?).map_err(|_| ErrorKind::InvalidKeyFormat.into())
}

// ---------------------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------------------

fn new_signer(algorithm: &Algorithm, key: &EncodingKey) -> Result<Box<dyn JwtSigner>> {
    let k = || Ok(key.as_bytes());
    Ok(match algorithm {
        Algorithm::EdDSA => Box::new(eddsa_signer(key)?),
        Algorithm::HS256 => Box::new(Hs256Signer(hmac_key(key.family(), k())?)),
        Algorithm::HS384 => Box::new(Hs384Signer(hmac_key(key.family(), k())?)),
        Algorithm::HS512 => Box::new(Hs512Signer(hmac_key(key.family(), k())?)),
        _ => return Err(new_error(ErrorKind::InvalidAlgorithm)),
    })
}

fn new_verifier(algorithm: &Algorithm, key: &DecodingKey) -> Result<Box<dyn JwtVerifier>> {
    Ok(match algorithm {
        Algorithm::EdDSA => Box::new(eddsa_verifier(key)?),
        Algorithm::HS256 => Box::new(Hs256Verifier(hmac_key(
            key.family(),
            key.try_get_as_bytes(),
        )?)),
        Algorithm::HS384 => Box::new(Hs384Verifier(hmac_key(
            key.family(),
            key.try_get_as_bytes(),
        )?)),
        Algorithm::HS512 => Box::new(Hs512Verifier(hmac_key(
            key.family(),
            key.try_get_as_bytes(),
        )?)),
        _ => return Err(new_error(ErrorKind::InvalidAlgorithm)),
    })
}

fn unsupported_rsa(_: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    Err(new_error(ErrorKind::InvalidAlgorithm))
}

fn unsupported_ec(_: &[u8], _: Algorithm) -> Result<(EllipticCurve, Vec<u8>, Vec<u8>)> {
    Err(new_error(ErrorKind::InvalidAlgorithm))
}

fn ed_public_from_private(der: &[u8], curve: &EllipticCurve) -> Result<Vec<u8>> {
    match curve {
        EllipticCurve::Ed25519 => Ok(ed25519_dalek::SigningKey::from_pkcs8_der(der)
            .map_err(|_| ErrorKind::InvalidEddsaKey)?
            .verifying_key()
            .as_bytes()
            .to_vec()),
        _ => Err(new_error(ErrorKind::InvalidAlgorithm)),
    }
}

fn digest(data: &[u8], hash: ThumbprintHash) -> Result<Vec<u8>> {
    Ok(match hash {
        ThumbprintHash::SHA256 => Sha256::digest(data).to_vec(),
        ThumbprintHash::SHA384 => Sha384::digest(data).to_vec(),
        ThumbprintHash::SHA512 => Sha512::digest(data).to_vec(),
        _ => return Err(new_error(ErrorKind::InvalidAlgorithm)),
    })
}

/// The CF Meet `jsonwebtoken` provider: EdDSA and HS256 / HS384 / HS512 only.
pub static PROVIDER: CryptoProvider = CryptoProvider {
    signer_factory: new_signer,
    verifier_factory: new_verifier,
    key_utils: KeyUtils {
        rsa_pub_components_from_private_key: unsupported_rsa,
        rsa_pub_components_from_public_key: unsupported_rsa,
        ec_pub_components_from_private_key: unsupported_ec,
        ed_pub_components_from_private_key: ed_public_from_private,
        compute_digest: digest,
    },
};

/// Installs [`PROVIDER`] as the process-wide `jsonwebtoken` provider (idempotent).
///
/// Panics if a DIFFERENT provider was installed first: a process must never sign or verify
/// tokens with algorithms CF Meet does not allow.
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if let Err(existing) = PROVIDER.install_default() {
            assert!(
                std::ptr::eq(existing, &PROVIDER),
                "another jsonwebtoken CryptoProvider was installed before cf-service-auth's"
            );
        }
    });
}

/// [`jsonwebtoken::encode`] with the CF Meet provider installed.
pub fn encode<T: Serialize>(header: &Header, claims: &T, key: &EncodingKey) -> Result<String> {
    install();
    jsonwebtoken::encode(header, claims, key)
}

/// [`jsonwebtoken::decode`] with the CF Meet provider installed.
pub fn decode<T: DeserializeOwned>(
    token: impl AsRef<[u8]>,
    key: &DecodingKey,
    validation: &Validation,
) -> Result<TokenData<T>> {
    install();
    jsonwebtoken::decode(token, key, validation)
}

/// [`jsonwebtoken::crypto::sign`] (a raw JWS signature, base64url) with the CF Meet
/// provider installed.
pub fn sign(message: &[u8], key: &EncodingKey, algorithm: Algorithm) -> Result<String> {
    install();
    jsonwebtoken::crypto::sign(message, key, algorithm)
}

/// [`jsonwebtoken::decode_header`] (no cryptography; provided for a single import path).
pub fn decode_header(token: impl AsRef<[u8]>) -> Result<Header> {
    jsonwebtoken::decode_header(token)
}

/// `Error` re-exported for callers that only use this module.
pub type JwtError = Error;
