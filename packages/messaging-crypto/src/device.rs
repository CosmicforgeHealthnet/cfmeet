//! Client-side device cryptography (feature `client`). PRIVATE keys live here.
//!
//! A [`Device`] holds one Ed25519 MLS signature key pair and an in-memory OpenMLS
//! provider (whose storage keeps the HPKE private keys of the KeyPackages it generated).
//! Durable, platform-protected device storage (Keychain / Keystore / IndexedDB with a
//! non-extractable wrapping key) is the client SDKs' responsibility (Part V Appendix L);
//! this module is the shared core they wrap.
//!
//! [`Debug`] never prints key material.

use std::fmt;

use openmls::prelude::{
    tls_codec::Serialize as _, ApplicationIdExtension, BasicCredential, CredentialWithKey,
    Extension, Extensions, KeyPackage, Lifetime, MlsMessageOut,
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use uuid::Uuid;

use crate::{binding, profile::MANDATORY_CIPHERSUITE};

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("key generation failed")]
    KeyGeneration,
    #[error("KeyPackage creation failed")]
    KeyPackage,
}

/// One client device.
pub struct Device {
    device_id: Uuid,
    signer: SignatureKeyPair,
    provider: OpenMlsRustCrypto,
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Device")
            .field("device_id", &self.device_id)
            .field("signature_key", &"[public key omitted]")
            .finish_non_exhaustive()
    }
}

impl Device {
    /// Generates a fresh device signature key pair for the server-assigned `device_id`.
    pub fn generate(device_id: Uuid) -> Result<Self, DeviceError> {
        let signer = SignatureKeyPair::new(MANDATORY_CIPHERSUITE.signature_algorithm())
            .map_err(|_| DeviceError::KeyGeneration)?;
        Ok(Self {
            device_id,
            signer,
            provider: OpenMlsRustCrypto::default(),
        })
    }

    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    /// The device signature PUBLIC key (Ed25519, 32 bytes).
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.signer.to_public_vec()
    }

    fn credential(&self) -> CredentialWithKey {
        CredentialWithKey {
            credential: BasicCredential::new(binding::device_credential_identity(self.device_id))
                .into(),
            signature_key: self.signer.to_public_vec().into(),
        }
    }

    fn build(
        &self,
        leaf_extensions: Option<Extensions<openmls::prelude::LeafNode>>,
        lifetime: Option<Lifetime>,
    ) -> Result<Vec<u8>, DeviceError> {
        let mut builder = KeyPackage::builder();
        if let Some(e) = leaf_extensions {
            builder = builder.leaf_node_extensions(e);
        }
        if let Some(l) = lifetime {
            builder = builder.key_package_lifetime(l);
        }
        let bundle = builder
            .build(
                MANDATORY_CIPHERSUITE,
                &self.provider,
                &self.signer,
                self.credential(),
            )
            .map_err(|_| DeviceError::KeyPackage)?;
        MlsMessageOut::from(bundle.key_package().clone())
            .tls_serialize_detached()
            .map_err(|_| DeviceError::KeyPackage)
    }

    /// The registration KeyPackage answering a server challenge: its leaf carries the
    /// registration binding as `application_id`, signed by the device key (RFC 9420 §7.2).
    pub fn registration_key_package(
        &self,
        challenge_id: Uuid,
        nonce: &[u8; binding::NONCE_LEN],
    ) -> Result<Vec<u8>, DeviceError> {
        let extension = Extension::ApplicationId(ApplicationIdExtension::new(
            &binding::registration_binding(challenge_id, nonce),
        ));
        let extensions = Extensions::single(extension).map_err(|_| DeviceError::KeyPackage)?;
        self.build(Some(extensions), None)
    }

    /// One ordinary KeyPackage for publication (OpenMLS default lifetime).
    pub fn key_package(&self) -> Result<Vec<u8>, DeviceError> {
        self.build(None, None)
    }

    /// A KeyPackage with an explicit lifetime (Unix seconds; tests of lifetime policy).
    pub fn key_package_with_lifetime(
        &self,
        not_before: u64,
        not_after: u64,
    ) -> Result<Vec<u8>, DeviceError> {
        self.build(None, Some(Lifetime::init(not_before, not_after)))
    }
}
