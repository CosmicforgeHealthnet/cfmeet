//! The OpenMLS crypto provider of CF Meet: exactly the primitives of the one accepted
//! cipher suite, `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (profile.rs).
//!
//! It replaces `openmls_rust_crypto` / `openmls_basic_credential` 0.6.0, whose ECDSA
//! P-256 / P-384 support links `elliptic-curve 0.13` → `crypto-bigint 0.5`
//! (RUSTSEC-2026-0387). CF Meet never accepts a P-curve suite, so this provider leaves
//! them out. It is built on the same RustCrypto crates the former provider used, and its
//! outputs are the same bytes:
//!
//! * SHA-256, HKDF-SHA256, HMAC-SHA256 (`sha2`, `hkdf`, `hmac`);
//! * AES-128-GCM (RustCrypto `aes-gcm`, through the HPKE backend's AEAD interface, so
//!   AES-GCM stays a transitive dependency as before: the crate itself is reserved for
//!   Privacy Relay's Contact Vault by the service-boundary guard);
//! * Ed25519 signatures with strict verification (`ed25519-dalek`);
//! * HPKE base mode DHKEM(X25519, HKDF-SHA256) / HKDF-SHA256 with AES-128-GCM (the
//!   suite) or ChaCha20-Poly1305 (CF Meet's history transfer, history_transfer.rs),
//!   RFC 9180: `hpke-rs` 0.8 with its RustCrypto backend, the release line after the 0.7
//!   the former provider used (which links P-256 0.13); 0.8 links `crypto-bigint 0.7`.
//!
//! Every other algorithm is refused with an `Unsupported*` error, never computed.
//! Randomness comes from the operating system (`getrandom`; Web Crypto in browsers).
//!
//! Compatibility with the former provider is proven by golden vectors it produced
//! (`tests/provider_compatibility.rs`, `tests/vectors/provider_compat_v1.json`): the same
//! primitive outputs, and device states, groups and queued messages it persisted still
//! load, decrypt and continue.
//!
//! The function bodies follow `openmls_rust_crypto` 0.6.0 (MIT, Copyright (c) 2020
//! Franziskus Kiefer and the OpenMLS authors).

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use hpke_rs::{Hpke, HpkeError, Mode};
use hpke_rs_crypto::{
    error::Error as HpkeCryptoError,
    types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm},
    HpkeCrypto,
};
use hpke_rs_rust_crypto::HpkeRustCrypto;
use openmls_traits::{
    crypto::OpenMlsCrypto,
    random::OpenMlsRand,
    types::{
        AeadType, Ciphersuite, CryptoError, ExporterSecret, HashType, HpkeAeadType, HpkeCiphertext,
        HpkeConfig, HpkeKdfType, HpkeKemType, HpkeKeyPair, SignatureScheme,
    },
};
use sha2::{Digest, Sha256};
use tls_codec::SecretVLBytes;

/// The one cipher suite this provider implements: the profile's.
pub const SUITE: Ciphersuite = crate::profile::MANDATORY_CIPHERSUITE;

const ED25519_KEY_LEN: usize = 32;

/// Stateless crypto and randomness for OpenMLS (the backend uses it to verify; clients
/// also generate and use keys through [`CfMlsProvider`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct CfMlsCrypto;

fn sha256_only(hash_type: HashType) -> Result<(), CryptoError> {
    match hash_type {
        HashType::Sha2_256 => Ok(()),
        _ => Err(CryptoError::UnsupportedHashAlgorithm),
    }
}

fn aes128gcm_only(alg: AeadType) -> Result<(), CryptoError> {
    match alg {
        AeadType::Aes128Gcm => Ok(()),
        _ => Err(CryptoError::UnsupportedAeadAlgorithm),
    }
}

fn ed25519_only(alg: SignatureScheme) -> Result<(), CryptoError> {
    match alg {
        SignatureScheme::ED25519 => Ok(()),
        _ => Err(CryptoError::UnsupportedSignatureScheme),
    }
}

/// Base-mode HPKE with DHKEM(X25519, HKDF-SHA256) and HKDF-SHA256: AES-128-GCM (the
/// suite) or ChaCha20-Poly1305 (history transfer).
fn hpke(config: HpkeConfig) -> Result<Hpke<HpkeRustCrypto>, CryptoError> {
    match config {
        HpkeConfig(HpkeKemType::DhKem25519, HpkeKdfType::HkdfSha256, aead) => {
            let aead = match aead {
                HpkeAeadType::AesGcm128 => AeadAlgorithm::Aes128Gcm,
                HpkeAeadType::ChaCha20Poly1305 => AeadAlgorithm::ChaCha20Poly1305,
                _ => return Err(CryptoError::UnsupportedAeadAlgorithm),
            };
            Hpke::<HpkeRustCrypto>::try_new(
                Mode::Base,
                KemAlgorithm::DhKem25519,
                KdfAlgorithm::HkdfSha256,
                aead,
            )
            .map_err(|_| CryptoError::InsufficientRandomness)
        }
        HpkeConfig(HpkeKemType::DhKem25519, _, _) => Err(CryptoError::UnsupportedKdf),
        _ => Err(CryptoError::UnsupportedCiphersuite),
    }
}

fn random_bytes(out: &mut [u8]) -> Result<(), RandError> {
    getrandom::fill(out).map_err(|_| RandError::NotEnoughRandomness)
}

impl OpenMlsCrypto for CfMlsCrypto {
    fn supports(&self, ciphersuite: Ciphersuite) -> Result<(), CryptoError> {
        if ciphersuite == SUITE {
            Ok(())
        } else {
            Err(CryptoError::UnsupportedCiphersuite)
        }
    }

    fn supported_ciphersuites(&self) -> Vec<Ciphersuite> {
        vec![SUITE]
    }

    fn hkdf_extract(
        &self,
        hash_type: HashType,
        salt: &[u8],
        ikm: &[u8],
    ) -> Result<SecretVLBytes, CryptoError> {
        sha256_only(hash_type)?;
        let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
        Ok(prk[..].into())
    }

    fn hmac(
        &self,
        hash_type: HashType,
        key: &[u8],
        message: &[u8],
    ) -> Result<SecretVLBytes, CryptoError> {
        sha256_only(hash_type)?;
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|_| CryptoError::InvalidLength)?;
        mac.update(message);
        Ok(mac.finalize().into_bytes()[..].into())
    }

    fn hkdf_expand(
        &self,
        hash_type: HashType,
        prk: &[u8],
        info: &[u8],
        okm_len: usize,
    ) -> Result<SecretVLBytes, CryptoError> {
        sha256_only(hash_type)?;
        let hkdf =
            Hkdf::<Sha256>::from_prk(prk).map_err(|_| CryptoError::HkdfOutputLengthInvalid)?;
        let mut okm = vec![0u8; okm_len];
        hkdf.expand(info, &mut okm)
            .map_err(|_| CryptoError::HkdfOutputLengthInvalid)?;
        Ok(okm.into())
    }

    fn hash(&self, hash_type: HashType, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        sha256_only(hash_type)?;
        Ok(Sha256::digest(data).to_vec())
    }

    fn aead_encrypt(
        &self,
        alg: AeadType,
        key: &[u8],
        data: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        aes128gcm_only(alg)?;
        if nonce.len() != alg.nonce_size() {
            return Err(CryptoError::InvalidLength);
        }
        HpkeRustCrypto::aead_seal(AeadAlgorithm::Aes128Gcm, key, nonce, aad, data)
            .map_err(|_| CryptoError::CryptoLibraryError)
    }

    fn aead_decrypt(
        &self,
        alg: AeadType,
        key: &[u8],
        ct_tag: &[u8],
        nonce: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        aes128gcm_only(alg)?;
        if nonce.len() != alg.nonce_size() {
            return Err(CryptoError::InvalidLength);
        }
        HpkeRustCrypto::aead_open(AeadAlgorithm::Aes128Gcm, key, nonce, aad, ct_tag).map_err(|e| {
            match e {
                HpkeCryptoError::AeadOpenError | HpkeCryptoError::AeadInvalidCiphertext => {
                    CryptoError::AeadDecryptionError
                }
                _ => CryptoError::CryptoLibraryError,
            }
        })
    }

    fn signature_key_gen(&self, alg: SignatureScheme) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
        ed25519_only(alg)?;
        let mut seed = zeroize::Zeroizing::new([0u8; ED25519_KEY_LEN]);
        random_bytes(seed.as_mut()).map_err(|_| CryptoError::InsufficientRandomness)?;
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        Ok((
            sk.as_bytes().to_vec(),
            sk.verifying_key().to_bytes().to_vec(),
        ))
    }

    fn verify_signature(
        &self,
        alg: SignatureScheme,
        data: &[u8],
        pk: &[u8],
        signature: &[u8],
    ) -> Result<(), CryptoError> {
        ed25519_only(alg)?;
        let key = ed25519_dalek::VerifyingKey::try_from(pk)
            .map_err(|_| CryptoError::CryptoLibraryError)?;
        let signature = <[u8; ed25519_dalek::SIGNATURE_LENGTH]>::try_from(signature)
            .map_err(|_| CryptoError::CryptoLibraryError)?;
        key.verify_strict(data, &ed25519_dalek::Signature::from_bytes(&signature))
            .map_err(|_| CryptoError::InvalidSignature)
    }

    fn sign(&self, alg: SignatureScheme, data: &[u8], key: &[u8]) -> Result<Vec<u8>, CryptoError> {
        ed25519_only(alg)?;
        let key = ed25519_dalek::SigningKey::try_from(key)
            .map_err(|_| CryptoError::CryptoLibraryError)?;
        Ok(ed25519_dalek::Signer::sign(&key, data).to_bytes().to_vec())
    }

    fn hpke_seal(
        &self,
        config: HpkeConfig,
        pk_r: &[u8],
        info: &[u8],
        aad: &[u8],
        ptxt: &[u8],
    ) -> Result<HpkeCiphertext, CryptoError> {
        let (kem_output, ciphertext) = hpke(config)?
            .seal(&pk_r.into(), info, aad, ptxt, None, None, None)
            .map_err(|e| match e {
                HpkeError::InvalidInput => CryptoError::InvalidLength,
                _ => CryptoError::CryptoLibraryError,
            })?;
        Ok(HpkeCiphertext {
            kem_output: kem_output.into(),
            ciphertext: ciphertext.into(),
        })
    }

    fn hpke_open(
        &self,
        config: HpkeConfig,
        input: &HpkeCiphertext,
        sk_r: &[u8],
        info: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        hpke(config)?
            .open(
                input.kem_output.as_slice(),
                &sk_r.into(),
                info,
                aad,
                input.ciphertext.as_slice(),
                None,
                None,
                None,
            )
            .map_err(|_| CryptoError::HpkeDecryptionError)
    }

    fn hpke_setup_sender_and_export(
        &self,
        config: HpkeConfig,
        pk_r: &[u8],
        info: &[u8],
        exporter_context: &[u8],
        exporter_length: usize,
    ) -> Result<(Vec<u8>, ExporterSecret), CryptoError> {
        let (kem_output, context) = hpke(config)?
            .setup_sender(&pk_r.into(), info, None, None, None)
            .map_err(|_| CryptoError::SenderSetupError)?;
        let exported = context
            .export(exporter_context, exporter_length)
            .map_err(|_| CryptoError::ExporterError)?;
        Ok((kem_output, exported.into()))
    }

    fn hpke_setup_receiver_and_export(
        &self,
        config: HpkeConfig,
        enc: &[u8],
        sk_r: &[u8],
        info: &[u8],
        exporter_context: &[u8],
        exporter_length: usize,
    ) -> Result<ExporterSecret, CryptoError> {
        let context = hpke(config)?
            .setup_receiver(enc, &sk_r.into(), info, None, None, None)
            .map_err(|_| CryptoError::ReceiverSetupError)?;
        let exported = context
            .export(exporter_context, exporter_length)
            .map_err(|_| CryptoError::ExporterError)?;
        Ok(exported.into())
    }

    fn derive_hpke_keypair(
        &self,
        config: HpkeConfig,
        ikm: &[u8],
    ) -> Result<HpkeKeyPair, CryptoError> {
        let (private, public) = hpke(config)?
            .derive_key_pair(ikm)
            .map_err(|e| match e {
                HpkeError::InvalidInput => CryptoError::InvalidLength,
                _ => CryptoError::CryptoLibraryError,
            })?
            .into_keys();
        Ok(HpkeKeyPair {
            private: private.as_slice().into(),
            public: public.as_slice().into(),
        })
    }
}

impl OpenMlsRand for CfMlsCrypto {
    type Error = RandError;

    fn random_array<const N: usize>(&self) -> Result<[u8; N], Self::Error> {
        let mut out = [0u8; N];
        random_bytes(&mut out)?;
        Ok(out)
    }

    fn random_vec(&self, len: usize) -> Result<Vec<u8>, Self::Error> {
        let mut out = vec![0u8; len];
        random_bytes(&mut out)?;
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RandError {
    #[error("the operating system random number generator failed")]
    NotEnoughRandomness,
}

#[cfg(feature = "client")]
pub use client::{CfMlsProvider, SignatureKeys};

#[cfg(feature = "client")]
mod client {
    use openmls_memory_storage::MemoryStorage;
    use openmls_traits::{
        crypto::OpenMlsCrypto as _,
        signatures::{Signer, SignerError},
        types::{CryptoError, SignatureScheme},
        OpenMlsProvider,
    };
    use tls_codec::{SecretVLBytes, TlsDeserialize, TlsDeserializeBytes, TlsSerialize, TlsSize};

    use super::CfMlsCrypto;

    /// A client's OpenMLS provider: [`CfMlsCrypto`] plus the in-memory key and group
    /// store (`openmls_memory_storage`, the store the former provider embedded, so
    /// persisted entries keep their encoding).
    #[derive(Debug, Default)]
    pub struct CfMlsProvider {
        crypto: CfMlsCrypto,
        storage: MemoryStorage,
    }

    impl OpenMlsProvider for CfMlsProvider {
        type CryptoProvider = CfMlsCrypto;
        type RandProvider = CfMlsCrypto;
        type StorageProvider = MemoryStorage;

        fn storage(&self) -> &Self::StorageProvider {
            &self.storage
        }

        fn crypto(&self) -> &Self::CryptoProvider {
            &self.crypto
        }

        fn rand(&self) -> &Self::RandProvider {
            &self.crypto
        }
    }

    /// A device's Ed25519 MLS signature key pair.
    ///
    /// Its TLS encoding — private key, public key (both variable-length vectors) and the
    /// signature scheme — is that of `openmls_basic_credential::SignatureKeyPair` 0.6.0,
    /// so device states exported before the provider change import unchanged. The
    /// private key is zeroized on drop; [`Debug`] never prints it.
    #[derive(TlsSerialize, TlsSize, TlsDeserialize, TlsDeserializeBytes)]
    pub struct SignatureKeys {
        private: SecretVLBytes,
        public: Vec<u8>,
        signature_scheme: SignatureScheme,
    }

    impl std::fmt::Debug for SignatureKeys {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SignatureKeys")
                .field("signature_scheme", &self.signature_scheme)
                .finish_non_exhaustive()
        }
    }

    impl SignatureKeys {
        /// A fresh key pair (Ed25519 only).
        pub fn generate(signature_scheme: SignatureScheme) -> Result<Self, CryptoError> {
            let (private, public) = CfMlsCrypto.signature_key_gen(signature_scheme)?;
            let private = zeroize::Zeroizing::new(private);
            Ok(Self {
                private: private.as_slice().into(),
                public,
                signature_scheme,
            })
        }

        pub fn public(&self) -> &[u8] {
            &self.public
        }

        pub fn to_public_vec(&self) -> Vec<u8> {
            self.public.clone()
        }
    }

    impl Signer for SignatureKeys {
        fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, SignerError> {
            CfMlsCrypto
                .sign(self.signature_scheme, payload, self.private.as_slice())
                .map_err(|_| SignerError::SigningError)
        }

        fn signature_scheme(&self) -> SignatureScheme {
            self.signature_scheme
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_accepted_suite_is_supported() {
        let c = CfMlsCrypto;
        assert_eq!(
            SUITE,
            Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519
        );
        assert_eq!(c.supported_ciphersuites(), vec![SUITE]);
        for suite in c.supported_ciphersuites() {
            assert!(c.supports(suite).is_ok());
        }
        for other in [
            Ciphersuite::MLS_128_DHKEMP256_AES128GCM_SHA256_P256,
            Ciphersuite::MLS_128_DHKEMX25519_CHACHA20POLY1305_SHA256_Ed25519,
            Ciphersuite::MLS_256_DHKEMP384_AES256GCM_SHA384_P384,
        ] {
            assert_eq!(c.supports(other), Err(CryptoError::UnsupportedCiphersuite));
        }
    }

    #[test]
    fn other_algorithms_are_refused_not_computed() {
        let c = CfMlsCrypto;
        assert_eq!(
            c.hash(HashType::Sha2_384, b"x"),
            Err(CryptoError::UnsupportedHashAlgorithm)
        );
        assert_eq!(
            c.aead_encrypt(AeadType::Aes256Gcm, &[0; 32], b"x", &[0; 12], b""),
            Err(CryptoError::UnsupportedAeadAlgorithm)
        );
        assert_eq!(
            c.signature_key_gen(SignatureScheme::ECDSA_SECP256R1_SHA256),
            Err(CryptoError::UnsupportedSignatureScheme)
        );
        assert_eq!(
            c.verify_signature(
                SignatureScheme::ECDSA_SECP256R1_SHA256,
                b"x",
                &[4; 65],
                &[0; 70]
            ),
            Err(CryptoError::UnsupportedSignatureScheme)
        );
        let p256 = HpkeConfig(
            HpkeKemType::DhKemP256,
            HpkeKdfType::HkdfSha256,
            HpkeAeadType::AesGcm128,
        );
        assert_eq!(
            c.derive_hpke_keypair(p256, &[1; 32]).err(),
            Some(CryptoError::UnsupportedCiphersuite)
        );
    }

    #[test]
    fn signatures_are_strict_and_bound_to_key_and_message() {
        let c = CfMlsCrypto;
        let (sk, pk) = c.signature_key_gen(SignatureScheme::ED25519).unwrap();
        let sig = c.sign(SignatureScheme::ED25519, b"m", &sk).unwrap();
        assert!(c
            .verify_signature(SignatureScheme::ED25519, b"m", &pk, &sig)
            .is_ok());
        assert_eq!(
            c.verify_signature(SignatureScheme::ED25519, b"n", &pk, &sig),
            Err(CryptoError::InvalidSignature)
        );
        let mut bad = sig.clone();
        bad[0] ^= 1;
        assert!(c
            .verify_signature(SignatureScheme::ED25519, b"m", &pk, &bad)
            .is_err());
        assert!(c
            .verify_signature(SignatureScheme::ED25519, b"m", &pk, &sig[..63])
            .is_err());
    }

    #[test]
    fn randomness_is_not_constant() {
        let c = CfMlsCrypto;
        let a: [u8; 32] = c.random_array().unwrap();
        let b: [u8; 32] = c.random_array().unwrap();
        assert_ne!(a, b);
        assert_eq!(c.random_vec(17).unwrap().len(), 17);
    }
}
