//! Version 2 transport framing. This is a collection of individually HPKE-sealed,
//! device-signed objects, not a new cipher. The encrypted manifest is object u32::MAX;
//! its exact plaintext SHA-256 binds every chunk. Clients must verify that digest and
//! the manifest's ordered plaintext digests, coverage and schema before importing.
//! Request approval / comparison-code verification is still required. In particular,
//! a same-account directory entry alone is not authority to disclose a history archive.

use super::{parse_version, verify, TransferContext};
#[cfg(feature = "client")]
use super::{CfMlsCrypto, TransferError, SIGNATURE_LEN};
#[cfg(feature = "client")]
use openmls::prelude::OpenMlsCrypto;

pub const VERSION: u8 = 2;
pub const LABEL: &[u8] = b"cfmeet-messaging-history-transfer:v2";
pub const MAX_OBJECT_PLAINTEXT: usize = 256 * 1024;
pub const MAX_OBJECT_SEALED: usize = MAX_OBJECT_PLAINTEXT + 121;
pub const MAX_CHUNKS: u32 = 4096;
pub const MAX_TOTAL_PLAINTEXT: u64 = 1024 * 1024 * 1024;
pub const MANIFEST_INDEX: u32 = u32::MAX;

/// Immutable descriptor authenticated in every object signature and HPKE AAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectContext {
    pub transfer: TransferContext,
    pub snapshot_id: uuid::Uuid,
    pub expires_at_unix: i64,
    pub manifest_digest: [u8; 32],
    pub chunk_count: u32,
    pub total_plaintext_bytes: u64,
    pub object_index: u32,
}

impl ObjectContext {
    pub fn valid(&self) -> bool {
        self.chunk_count > 0
            && self.chunk_count <= MAX_CHUNKS
            && self.total_plaintext_bytes > 0
            && self.total_plaintext_bytes <= MAX_TOTAL_PLAINTEXT
            && (self.object_index == MANIFEST_INDEX || self.object_index < self.chunk_count)
    }

    fn encode(&self, purpose: &[u8]) -> Vec<u8> {
        let mut out = self.transfer.encode(LABEL);
        out.extend_from_slice(&(purpose.len() as u32).to_be_bytes());
        out.extend_from_slice(purpose);
        out.extend_from_slice(self.snapshot_id.as_bytes());
        out.extend_from_slice(&self.expires_at_unix.to_be_bytes());
        out.extend_from_slice(&self.manifest_digest);
        out.extend_from_slice(&self.chunk_count.to_be_bytes());
        out.extend_from_slice(&self.total_plaintext_bytes.to_be_bytes());
        out.extend_from_slice(&self.object_index.to_be_bytes());
        out
    }
}

pub fn request_signing_input(
    ctx: &TransferContext,
    transfer_public_key: &[u8],
    expires_at_unix: i64,
) -> Vec<u8> {
    let mut out = ctx.encode(LABEL);
    out.extend_from_slice(b"request\0");
    out.extend_from_slice(&(transfer_public_key.len() as u32).to_be_bytes());
    out.extend_from_slice(transfer_public_key);
    out.extend_from_slice(&expires_at_unix.to_be_bytes());
    out
}

pub fn verify_request(
    target_key: &[u8],
    ctx: &TransferContext,
    transfer_public_key: &[u8],
    expires_at_unix: i64,
    signature: &[u8],
) -> bool {
    transfer_public_key.len() == super::TRANSFER_PUBLIC_KEY_LEN
        && verify(
            target_key,
            &request_signing_input(ctx, transfer_public_key, expires_at_unix),
            signature,
        )
}

fn signing_input(ctx: &ObjectContext, bytes: &[u8]) -> Vec<u8> {
    let mut out = ctx.encode(b"sealed");
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

pub fn verify_object(source_key: &[u8], ctx: &ObjectContext, sealed: &[u8]) -> bool {
    if !ctx.valid() || sealed.len() > MAX_OBJECT_SEALED {
        return false;
    }
    match parse_version(sealed, VERSION) {
        Ok(s) => {
            s.kem_output.len() == 32
                && s.ciphertext.len() >= 16
                && verify(source_key, &signing_input(ctx, s.signed), s.signature)
        }
        Err(_) => false,
    }
}

fn checkpoint_input(ctx: &ObjectContext, next_index: u32) -> Vec<u8> {
    let mut manifest = *ctx;
    manifest.object_index = MANIFEST_INDEX;
    let mut out = manifest.encode(b"durable-checkpoint");
    out.extend_from_slice(&next_index.to_be_bytes());
    out
}

pub fn verify_checkpoint(
    target_key: &[u8],
    ctx: &ObjectContext,
    next_index: u32,
    signature: &[u8],
) -> bool {
    ctx.valid()
        && next_index <= ctx.chunk_count
        && verify(target_key, &checkpoint_input(ctx, next_index), signature)
}

#[cfg(feature = "client")]
pub use client::*;

#[cfg(feature = "client")]
mod client {
    use super::*;
    use crate::{device::Device, history_transfer::TransferKeys};
    use openmls::prelude::HpkeCiphertext;
    use openmls_traits::signatures::Signer;

    pub fn sign_request(
        device: &Device,
        ctx: &TransferContext,
        keys: &TransferKeys,
        expires_at_unix: i64,
    ) -> Result<Vec<u8>, TransferError> {
        device
            .signer()
            .sign(&request_signing_input(
                ctx,
                keys.public_key(),
                expires_at_unix,
            ))
            .map_err(|_| TransferError::Signing)
    }

    pub fn sign_checkpoint(
        device: &Device,
        ctx: &ObjectContext,
        next_index: u32,
    ) -> Result<Vec<u8>, TransferError> {
        if !ctx.valid()
            || next_index > ctx.chunk_count
            || device.device_id() != ctx.transfer.target_device_id
        {
            return Err(TransferError::Malformed);
        }
        device
            .signer()
            .sign(&checkpoint_input(ctx, next_index))
            .map_err(|_| TransferError::Signing)
    }

    pub fn seal_object(
        device: &Device,
        ctx: &ObjectContext,
        transfer_public_key: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, TransferError> {
        if !ctx.valid()
            || transfer_public_key.len() != 32
            || plaintext.is_empty()
            || plaintext.len() > MAX_OBJECT_PLAINTEXT
        {
            return Err(TransferError::Malformed);
        }
        let ct = CfMlsCrypto
            .hpke_seal(
                super::super::HPKE,
                transfer_public_key,
                LABEL,
                &ctx.encode(b"aad"),
                plaintext,
            )
            .map_err(|_| TransferError::Sealing)?;
        let (kem, body) = (ct.kem_output.as_slice(), ct.ciphertext.as_slice());
        let mut out = Vec::with_capacity(9 + kem.len() + body.len() + SIGNATURE_LEN);
        out.push(VERSION);
        out.extend_from_slice(&(kem.len() as u32).to_be_bytes());
        out.extend_from_slice(kem);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        let signature = device
            .signer()
            .sign(&signing_input(ctx, &out))
            .map_err(|_| TransferError::Signing)?;
        out.extend_from_slice(&signature);
        Ok(out)
    }

    /// Opening does not consume a transfer key: every object and retry needs it.
    /// The owner clears its checkpoint after verified durable import or termination.
    pub fn open_object(
        keys: &TransferKeys,
        source_key: &[u8],
        ctx: &ObjectContext,
        sealed: &[u8],
    ) -> Result<Vec<u8>, TransferError> {
        if !verify_object(source_key, ctx, sealed) {
            return Err(TransferError::BadSignature);
        }
        let s = parse_version(sealed, VERSION)?;
        let ciphertext = HpkeCiphertext {
            kem_output: s.kem_output.to_vec().into(),
            ciphertext: s.ciphertext.to_vec().into(),
        };
        let out = CfMlsCrypto
            .hpke_open(
                super::super::HPKE,
                &ciphertext,
                keys.private_key(),
                LABEL,
                &ctx.encode(b"aad"),
            )
            .map_err(|_| TransferError::Open)?;
        if out.is_empty() || out.len() > MAX_OBJECT_PLAINTEXT {
            return Err(TransferError::Malformed);
        }
        Ok(out)
    }
}

#[cfg(all(test, feature = "client"))]
mod tests {
    use super::*;
    use crate::{device::Device, history_transfer::TransferKeys};
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    #[test]
    fn large_transfer_resumes_with_bound_independent_objects() {
        let transfer = TransferContext {
            transfer_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            principal_id: Uuid::new_v4(),
            source_device_id: Uuid::new_v4(),
            target_device_id: Uuid::new_v4(),
        };
        let source = Device::generate(transfer.source_device_id).unwrap();
        let target = Device::generate(transfer.target_device_id).unwrap();
        let keys = TransferKeys::generate().unwrap();
        let expires = 1_900_000_000;
        let request = sign_request(&target, &transfer, &keys, expires).unwrap();
        assert!(verify_request(
            &target.signature_public_key(),
            &transfer,
            keys.public_key(),
            expires,
            &request
        ));
        assert!(
            !super::super::verify_request(
                &target.signature_public_key(),
                &transfer,
                keys.public_key(),
                expires,
                &request
            ),
            "no protocol downgrade"
        );
        let manifest =
            b"example authenticated archive manifest with ordered chunk plaintext digests";
        let mut c = ObjectContext {
            transfer,
            snapshot_id: Uuid::new_v4(),
            expires_at_unix: expires,
            manifest_digest: Sha256::digest(manifest).into(),
            chunk_count: 4,
            total_plaintext_bytes: 4 * MAX_OBJECT_PLAINTEXT as u64,
            object_index: MANIFEST_INDEX,
        };
        let sealed = seal_object(&source, &c, keys.public_key(), manifest).unwrap();
        assert_eq!(
            open_object(&keys, &source.signature_public_key(), &c, &sealed).unwrap(),
            manifest
        );
        let checkpoint = keys.export_secret();
        drop(keys);
        let keys = TransferKeys::import_secret(&checkpoint).unwrap();
        let mut total = 0;
        for i in 0..c.chunk_count {
            c.object_index = i;
            let plaintext = vec![i as u8; MAX_OBJECT_PLAINTEXT];
            let sealed = seal_object(&source, &c, keys.public_key(), &plaintext).unwrap();
            assert_eq!(
                open_object(&keys, &source.signature_public_key(), &c, &sealed).unwrap(),
                plaintext
            );
            assert_eq!(
                open_object(&keys, &source.signature_public_key(), &c, &sealed).unwrap(),
                plaintext,
                "idempotent retry"
            );
            let mut wrong = c;
            wrong.object_index = (i + 1) % c.chunk_count;
            assert!(open_object(&keys, &source.signature_public_key(), &wrong, &sealed).is_err());
            wrong = c;
            wrong.expires_at_unix += 1;
            assert!(open_object(&keys, &source.signature_public_key(), &wrong, &sealed).is_err());
            wrong = c;
            wrong.manifest_digest[0] ^= 1;
            assert!(open_object(&keys, &source.signature_public_key(), &wrong, &sealed).is_err());
            wrong = c;
            wrong.transfer.target_device_id = Uuid::new_v4();
            assert!(open_object(&keys, &source.signature_public_key(), &wrong, &sealed).is_err());
            let mut corrupt = sealed.clone();
            corrupt[64] ^= 1;
            assert!(open_object(&keys, &source.signature_public_key(), &c, &corrupt).is_err());
            total += plaintext.len();
        }
        assert!(total > 512 * 1024);
        assert!(TransferKeys::import_secret(&checkpoint[..38]).is_err());
        assert!(seal_object(&source, &c, keys.public_key(), &[]).is_err());
        assert!(seal_object(
            &source,
            &c,
            keys.public_key(),
            &vec![0; MAX_OBJECT_PLAINTEXT + 1]
        )
        .is_err());
    }
}
