//! Authenticated device-to-device history transfer (ADR 0052 AD-07): how a principal's
//! NEW device obtains the conversation history its EXISTING device holds, with no server
//! key escrow and no server-side recovery key.
//!
//! 1. The target (new) device generates a single-use HPKE key pair ([`TransferKeys`],
//!    feature `client`) and signs a request binding the transfer id, the ownership scope,
//!    both device ids, the transfer public key and an expiry with its DEVICE signature key
//!    ([`sign_request`]). The server checks it ([`verify_request`]) but cannot forge it:
//!    the device key is published in the key-transparency log.
//! 2. The source (existing) device verifies the request against the target device key it
//!    obtained from the transparency-backed directory, and both devices display the same
//!    [`comparison_code`] for the user to compare — this binds the transfer to the two
//!    physical devices even against a server that registered a rogue device.
//! 3. The source seals the history archive to the transfer public key (RFC 9180 HPKE,
//!    DHKEM(X25519, HKDF-SHA256) / HKDF-SHA256 / ChaCha20-Poly1305 through the OpenMLS
//!    RustCrypto provider; the transfer context is the AAD) and signs the sealed bytes
//!    with its device key ([`seal_history`]). The server stores only these opaque bytes
//!    (it can check the signature: [`verify_sealed`]) and deletes them on completion or
//!    expiry.
//! 4. The target verifies the source device signature and opens the archive with its
//!    transfer private key ([`open_history`]), which then disappears with [`TransferKeys`].
//!
//! The archive's content format (which conversations, messages and attachment keys) is
//! the client SDK's; this module only guarantees confidentiality to the target device,
//! authenticity from the source device and binding to the transfer.

use openmls::prelude::{
    HpkeAeadType, HpkeConfig, HpkeKdfType, HpkeKemType, OpenMlsCrypto, SignatureScheme,
};
use openmls_rust_crypto::RustCrypto;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Domain separation of every transfer signature and of the HPKE `info`.
pub const LABEL: &[u8] = b"cfmeet-messaging-history-transfer:v1";
/// Format version of a sealed history.
pub const SEALED_VERSION: u8 = 1;
/// Length of an X25519 transfer public key.
pub const TRANSFER_PUBLIC_KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub(crate) const HPKE: HpkeConfig = HpkeConfig(
    HpkeKemType::DhKem25519,
    HpkeKdfType::HkdfSha256,
    HpkeAeadType::ChaCha20Poly1305,
);

/// Everything a transfer is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferContext {
    pub transfer_id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub principal_id: Uuid,
    pub source_device_id: Uuid,
    pub target_device_id: Uuid,
}

impl TransferContext {
    /// Length-prefixed (u32 BE) label and fields.
    pub fn encode(&self, purpose: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(160);
        let ids = [
            self.transfer_id,
            self.tenant_id,
            self.application_id,
            self.environment_id,
            self.principal_id,
            self.source_device_id,
            self.target_device_id,
        ];
        let mut push = |f: &[u8]| {
            out.extend_from_slice(&(f.len() as u32).to_be_bytes());
            out.extend_from_slice(f);
        };
        push(LABEL);
        push(purpose);
        for id in ids {
            push(id.as_bytes());
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    #[error("key generation failed")]
    KeyGeneration,
    #[error("signing failed")]
    Signing,
    #[error("sealing failed")]
    Sealing,
    #[error("the sealed history is malformed")]
    Malformed,
    #[error("the source device signature does not verify")]
    BadSignature,
    #[error("the history cannot be opened with this transfer key")]
    Open,
}

/// The bytes the TARGET device signs to request a transfer.
pub fn request_signing_input(
    ctx: &TransferContext,
    transfer_public_key: &[u8],
    expires_at_unix: i64,
) -> Vec<u8> {
    let mut out = ctx.encode(b"request");
    out.extend_from_slice(&(transfer_public_key.len() as u32).to_be_bytes());
    out.extend_from_slice(transfer_public_key);
    out.extend_from_slice(&expires_at_unix.to_be_bytes());
    out
}

fn verify(public_key: &[u8], input: &[u8], signature: &[u8]) -> bool {
    signature.len() == SIGNATURE_LEN
        && RustCrypto::default()
            .verify_signature(SignatureScheme::ED25519, input, public_key, signature)
            .is_ok()
}

/// Whether the target device (Ed25519 `target_device_public_key`) signed this request.
pub fn verify_request(
    target_device_public_key: &[u8],
    ctx: &TransferContext,
    transfer_public_key: &[u8],
    expires_at_unix: i64,
    signature: &[u8],
) -> bool {
    transfer_public_key.len() == TRANSFER_PUBLIC_KEY_LEN
        && verify(
            target_device_public_key,
            &request_signing_input(ctx, transfer_public_key, expires_at_unix),
            signature,
        )
}

/// The 8-digit code both devices display (grouped `1234 5678`): SHA-256 over the context,
/// the transfer key and both device keys. Equal codes mean both devices see the same
/// transfer between the same two device keys.
pub fn comparison_code(
    ctx: &TransferContext,
    transfer_public_key: &[u8],
    source_device_public_key: &[u8],
    target_device_public_key: &[u8],
) -> String {
    let mut h = Sha256::new();
    h.update(ctx.encode(b"comparison"));
    for k in [
        transfer_public_key,
        source_device_public_key,
        target_device_public_key,
    ] {
        h.update((k.len() as u32).to_be_bytes());
        h.update(k);
    }
    let d = h.finalize();
    let n = u64::from_be_bytes(d[..8].try_into().expect("8 bytes")) % 100_000_000;
    format!("{:04} {:04}", n / 10_000, n % 10_000)
}

/// A parsed sealed history: `version || u32 kem_len || kem || u32 ct_len || ct || sig64`.
#[cfg_attr(not(feature = "client"), allow(dead_code))]
pub(crate) struct Sealed<'a> {
    pub kem_output: &'a [u8],
    pub ciphertext: &'a [u8],
    pub signed: &'a [u8],
    pub signature: &'a [u8],
}

pub(crate) fn parse(sealed: &[u8]) -> Result<Sealed<'_>, TransferError> {
    let take = |b: &[u8], at: usize| -> Option<(usize, usize)> {
        let len = u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize;
        let start = at + 4;
        (b.len() >= start + len).then_some((start, start + len))
    };
    if sealed.first() != Some(&SEALED_VERSION) || sealed.len() < 1 + 8 + SIGNATURE_LEN {
        return Err(TransferError::Malformed);
    }
    let (k0, k1) = take(sealed, 1).ok_or(TransferError::Malformed)?;
    let (c0, c1) = take(sealed, k1).ok_or(TransferError::Malformed)?;
    if sealed.len() != c1 + SIGNATURE_LEN {
        return Err(TransferError::Malformed);
    }
    Ok(Sealed {
        kem_output: &sealed[k0..k1],
        ciphertext: &sealed[c0..c1],
        signed: &sealed[..c1],
        signature: &sealed[c1..],
    })
}

fn sealed_signing_input(ctx: &TransferContext, signed: &[u8]) -> Vec<u8> {
    let mut out = ctx.encode(b"sealed");
    out.extend_from_slice(&(signed.len() as u64).to_be_bytes());
    out.extend_from_slice(signed);
    out
}

/// Whether `sealed` is well-formed and signed by the source device for this transfer
/// (the server checks this before storing; it learns nothing about the content).
pub fn verify_sealed(
    source_device_public_key: &[u8],
    ctx: &TransferContext,
    sealed: &[u8],
) -> bool {
    match parse(sealed) {
        Ok(s) => verify(
            source_device_public_key,
            &sealed_signing_input(ctx, s.signed),
            s.signature,
        ),
        Err(_) => false,
    }
}

#[cfg(feature = "client")]
pub use client::*;

#[cfg(feature = "client")]
mod client {
    use openmls::prelude::{HpkeCiphertext, HpkePrivateKey, OpenMlsRand};
    use openmls_traits::signatures::Signer;

    use super::*;
    use crate::device::Device;

    /// The target device's single-use transfer key pair. The private half never leaves
    /// this value (no serialisation, no Debug) and is zeroized when it is dropped.
    pub struct TransferKeys {
        private: HpkePrivateKey,
        public: Vec<u8>,
    }

    impl TransferKeys {
        pub fn generate() -> Result<Self, TransferError> {
            let crypto = RustCrypto::default();
            let ikm = zeroize::Zeroizing::new(
                crypto
                    .random_array::<32>()
                    .map_err(|_| TransferError::KeyGeneration)?,
            );
            let kp = crypto
                .derive_hpke_keypair(HPKE, ikm.as_ref())
                .map_err(|_| TransferError::KeyGeneration)?;
            Ok(Self {
                private: kp.private,
                public: kp.public,
            })
        }

        pub fn public_key(&self) -> &[u8] {
            &self.public
        }
    }

    fn sign(device: &Device, input: &[u8]) -> Result<Vec<u8>, TransferError> {
        device
            .signer()
            .sign(input)
            .map_err(|_| TransferError::Signing)
    }

    /// The target device's signed transfer request.
    pub fn sign_request(
        target: &Device,
        ctx: &TransferContext,
        keys: &TransferKeys,
        expires_at_unix: i64,
    ) -> Result<Vec<u8>, TransferError> {
        sign(
            target,
            &request_signing_input(ctx, &keys.public, expires_at_unix),
        )
    }

    /// The source device seals `history` to the target's transfer key and signs it.
    pub fn seal_history(
        source: &Device,
        ctx: &TransferContext,
        transfer_public_key: &[u8],
        history: &[u8],
    ) -> Result<Vec<u8>, TransferError> {
        if transfer_public_key.len() != TRANSFER_PUBLIC_KEY_LEN {
            return Err(TransferError::Sealing);
        }
        let ct = RustCrypto::default()
            .hpke_seal(
                HPKE,
                transfer_public_key,
                LABEL,
                &ctx.encode(b"aad"),
                history,
            )
            .map_err(|_| TransferError::Sealing)?;
        let (kem, body) = (ct.kem_output.as_slice(), ct.ciphertext.as_slice());
        let mut out = Vec::with_capacity(9 + kem.len() + body.len() + SIGNATURE_LEN);
        out.push(SEALED_VERSION);
        out.extend_from_slice(&(kem.len() as u32).to_be_bytes());
        out.extend_from_slice(kem);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        let sig = sign(source, &sealed_signing_input(ctx, &out))?;
        out.extend_from_slice(&sig);
        Ok(out)
    }

    /// The target device verifies the source signature, then opens the history.
    pub fn open_history(
        keys: &TransferKeys,
        source_device_public_key: &[u8],
        ctx: &TransferContext,
        sealed: &[u8],
    ) -> Result<Vec<u8>, TransferError> {
        let s = parse(sealed)?;
        if !verify(
            source_device_public_key,
            &sealed_signing_input(ctx, s.signed),
            s.signature,
        ) {
            return Err(TransferError::BadSignature);
        }
        let input = HpkeCiphertext {
            kem_output: s.kem_output.to_vec().into(),
            ciphertext: s.ciphertext.to_vec().into(),
        };
        RustCrypto::default()
            .hpke_open(
                HPKE,
                &input,
                keys.private.as_ref(),
                LABEL,
                &ctx.encode(b"aad"),
            )
            .map_err(|_| TransferError::Open)
    }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    use crate::device::Device;

    fn ctx() -> TransferContext {
        TransferContext {
            transfer_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            principal_id: Uuid::new_v4(),
            source_device_id: Uuid::new_v4(),
            target_device_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn a_new_device_receives_history_only_it_can_open_and_only_from_the_source() {
        let c = ctx();
        let source = Device::generate(c.source_device_id).unwrap();
        let target = Device::generate(c.target_device_id).unwrap();
        let keys = TransferKeys::generate().unwrap();
        let exp = 1_900_000_000;
        let req = sign_request(&target, &c, &keys, exp).unwrap();
        let tpk = target.signature_public_key();
        let spk = source.signature_public_key();
        assert!(verify_request(&tpk, &c, keys.public_key(), exp, &req));
        assert!(
            !verify_request(&spk, &c, keys.public_key(), exp, &req),
            "another device key"
        );
        assert!(
            !verify_request(&tpk, &c, keys.public_key(), exp + 1, &req),
            "another expiry"
        );
        let other = TransferKeys::generate().unwrap();
        assert!(
            !verify_request(&tpk, &c, other.public_key(), exp, &req),
            "a swapped transfer key"
        );
        // Both devices compute the same comparison code; any substitution changes it.
        let code = comparison_code(&c, keys.public_key(), &spk, &tpk);
        assert_eq!(code.len(), 9);
        assert_ne!(code, comparison_code(&c, other.public_key(), &spk, &tpk));

        let history = b"conversation archive: 3 conversations, 120 messages".to_vec();
        let sealed = seal_history(&source, &c, keys.public_key(), &history).unwrap();
        assert!(
            !sealed.windows(12).any(|w| w == b"conversation"),
            "ciphertext only"
        );
        assert!(verify_sealed(&spk, &c, &sealed));
        assert!(!verify_sealed(&tpk, &c, &sealed));
        assert_eq!(open_history(&keys, &spk, &c, &sealed).unwrap(), history);
        // Another transfer key, another context, a tampered byte, a forged signer: refused.
        assert_eq!(
            open_history(&other, &spk, &c, &sealed),
            Err(TransferError::Open)
        );
        let mut c2 = c;
        c2.target_device_id = Uuid::new_v4();
        assert_eq!(
            open_history(&keys, &spk, &c2, &sealed),
            Err(TransferError::BadSignature)
        );
        let mut tampered = sealed.clone();
        let mid = tampered.len() / 2;
        tampered[mid] ^= 1;
        assert!(open_history(&keys, &spk, &c, &tampered).is_err());
        let forged = seal_history(&target, &c, keys.public_key(), b"injected").unwrap();
        assert_eq!(
            open_history(&keys, &spk, &c, &forged),
            Err(TransferError::BadSignature)
        );
        assert_eq!(
            open_history(&keys, &spk, &c, &sealed[..10]),
            Err(TransferError::Malformed)
        );
    }
}
