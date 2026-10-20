//! Client-side device cryptography (feature `client`). PRIVATE keys live here.
//!
//! A [`Device`] holds one Ed25519 MLS signature key pair and an in-memory OpenMLS
//! provider (whose storage keeps the HPKE private keys of the KeyPackages it generated).
//! Durable, platform-protected device storage (Keychain / Keystore / IndexedDB with a
//! non-extractable wrapping key) is the client SDKs' responsibility (Part V Appendix L);
//! this module is the shared core they wrap. [`Device::export_state`] /
//! [`Device::import_state`] give them the device's complete state (signature key pair,
//! KeyPackage private keys and every MLS group) as ONE opaque, versioned byte string that
//! the SDK must only ever store encrypted under such a wrapping key.
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
    /// An exported state that is malformed, of another version, of another device, or
    /// whose signature key pair does not sign and verify.
    #[error("invalid device state")]
    InvalidState,
}

/// Magic and version of [`Device::export_state`].
const STATE_MAGIC: &[u8; 8] = b"CFMDEV\x00\x01";
/// Upper bounds on an imported state (a device holds a few groups and KeyPackages).
const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STATE_ENTRIES: u32 = 1_000_000;

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

    pub(crate) fn provider(&self) -> &OpenMlsRustCrypto {
        &self.provider
    }

    pub(crate) fn signer(&self) -> &SignatureKeyPair {
        &self.signer
    }

    pub(crate) fn credential(&self) -> CredentialWithKey {
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

    /// The device's complete secret state: the signature key pair and the OpenMLS
    /// storage (KeyPackage private keys, every group's epoch secrets and tree). SECRET:
    /// store it only encrypted under a platform-protected, non-extractable key. The
    /// buffer is zeroized when dropped.
    pub fn export_state(&self) -> zeroize::Zeroizing<Vec<u8>> {
        use openmls::prelude::OpenMlsProvider as _;
        let values = self
            .provider
            .storage()
            .values
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let mut entries: Vec<(&Vec<u8>, &Vec<u8>)> = values.iter().collect();
        entries.sort();
        let mut out = zeroize::Zeroizing::new(Vec::new());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(self.device_id.as_bytes());
        // TLS encoding of the key pair (private key, public key, scheme).
        let signer = zeroize::Zeroizing::new(
            self.signer
                .tls_serialize_detached()
                .expect("a key pair always encodes"),
        );
        put(&mut out, &signer);
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (k, v) in entries {
            put(&mut out, k);
            put(&mut out, v);
        }
        out
    }

    /// Restores a device from [`Device::export_state`]. Refuses anything malformed or
    /// truncated, a state of another device than `expected_device_id`, and a key pair
    /// that does not produce a verifying signature.
    pub fn import_state(expected_device_id: Uuid, state: &[u8]) -> Result<Self, DeviceError> {
        use openmls::prelude::{OpenMlsCrypto as _, OpenMlsProvider as _};
        use openmls_traits::signatures::Signer as _;
        if state.len() > MAX_STATE_BYTES {
            return Err(DeviceError::InvalidState);
        }
        let mut r = Reader(state);
        if r.take(STATE_MAGIC.len())? != STATE_MAGIC {
            return Err(DeviceError::InvalidState);
        }
        let device_id = Uuid::from_slice(r.take(16)?).map_err(|_| DeviceError::InvalidState)?;
        if device_id != expected_device_id {
            return Err(DeviceError::InvalidState);
        }
        let signer = {
            use openmls::prelude::tls_codec::Deserialize as _;
            SignatureKeyPair::tls_deserialize_exact(r.bytes()?)
                .map_err(|_| DeviceError::InvalidState)?
        };
        let scheme = signer.signature_scheme();
        if scheme != MANDATORY_CIPHERSUITE.signature_algorithm() {
            return Err(DeviceError::InvalidState);
        }
        let public = signer.to_public_vec();
        let n = u32::from_be_bytes(r.take(4)?.try_into().expect("4 bytes"));
        if n > MAX_STATE_ENTRIES {
            return Err(DeviceError::InvalidState);
        }
        let provider = OpenMlsRustCrypto::default();
        {
            let mut values = provider
                .storage()
                .values
                .write()
                .unwrap_or_else(|p| p.into_inner());
            for _ in 0..n {
                let k = r.bytes()?.to_vec();
                let v = r.bytes()?.to_vec();
                if values.insert(k, v).is_some() {
                    return Err(DeviceError::InvalidState);
                }
            }
        }
        if !r.0.is_empty() {
            return Err(DeviceError::InvalidState);
        }
        // The pair must belong together: a fresh probe signature verifies with the public key.
        let probe = b"cfmeet device state import probe";
        let sig = signer.sign(probe).map_err(|_| DeviceError::InvalidState)?;
        provider
            .crypto()
            .verify_signature(scheme, probe, &public, &sig)
            .map_err(|_| DeviceError::InvalidState)?;
        Ok(Self {
            device_id,
            signer,
            provider,
        })
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

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DeviceError> {
        if self.0.len() < n {
            return Err(DeviceError::InvalidState);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn bytes(&mut self) -> Result<&'a [u8], DeviceError> {
        let n = u32::from_be_bytes(self.take(4)?.try_into().expect("4 bytes"));
        if n as usize > MAX_STATE_BYTES {
            return Err(DeviceError::InvalidState);
        }
        self.take(n as usize)
    }
}
