//! Client-side end-to-end encrypted attachments (feature `client`; Part V AD-08, §69–§80).
//!
//! The sending device:
//!
//! 1. generates a fresh random 256-bit attachment key ([`AttachmentKey::generate`]);
//! 2. encrypts the file BEFORE upload with XChaCha20-Poly1305 in the STREAM construction
//!    (Hoang, Reyhanitabar, Rogaway, Vizár — online AE, the RustCrypto `aead::stream`
//!    implementation, big-endian 32-bit chunk counter and last-chunk flag): every chunk is
//!    authenticated, chunks cannot be reordered, dropped, duplicated or truncated, and a
//!    random 19-byte nonce prefix under a single-use key means no (key, nonce) pair ever
//!    repeats;
//! 3. uploads only the ciphertext ([`EncryptedAttachment::ciphertext`]) through an upload
//!    intent; the server sees its size and SHA-256 and nothing else;
//! 4. sends the [`AttachmentDescriptor`] (key, ciphertext digest, sizes, media type,
//!    file name …) INSIDE an MLS application message, so only group members learn it.
//!
//! A recipient downloads the ciphertext with a server-issued grant and calls
//! [`decrypt_attachment`]: the ciphertext digest is checked first, then every chunk.
//! Every chunk's associated data binds the server-assigned attachment id, so a ciphertext
//! cannot be presented as another attachment.
//!
//! No primitive is implemented here; the format is versioned
//! ([`ATTACHMENT_FORMAT_V1`]). Key bytes are zeroized on drop and never printed.

use aead::rand_core::RngCore;
use aead::{
    stream::{DecryptorBE32, EncryptorBE32},
    KeyInit, OsRng,
};
use chacha20poly1305::XChaCha20Poly1305;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Format identifier carried by every descriptor.
pub const ATTACHMENT_FORMAT_V1: &str = "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1";
/// Plaintext bytes per chunk.
pub const CHUNK_SIZE: usize = 64 * 1024;
const TAG: usize = 16;
const NONCE_PREFIX: usize = 19;
const MAGIC: &[u8; 5] = b"CFMA1";
const HEADER: usize = MAGIC.len() + NONCE_PREFIX;
/// Largest attachment this module encrypts (bounded memory per call).
pub const MAX_PLAINTEXT: usize = 2 * 1024 * 1024 * 1024 - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttachmentError {
    #[error("attachment too large")]
    TooLarge,
    #[error("the ciphertext does not match the descriptor's digest")]
    DigestMismatch,
    #[error("the attachment failed authentication (tampered, truncated or reordered)")]
    Authentication,
    #[error("malformed attachment or descriptor")]
    Malformed,
}

/// A single-use attachment key (zeroized on drop; never printed).
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentKey([u8; 32]);

impl Drop for AttachmentKey {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl std::fmt::Debug for AttachmentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttachmentKey([redacted])")
    }
}

impl AttachmentKey {
    pub fn generate() -> Self {
        let mut k = [0u8; 32];
        OsRng.fill_bytes(&mut k);
        Self(k)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Everything a recipient needs to fetch and decrypt one attachment. Travels ONLY inside
/// an MLS-protected message (never in server-readable metadata).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDescriptor {
    pub format: String,
    pub attachment_id: Uuid,
    pub key: AttachmentKey,
    /// Lower-case hex SHA-256 of the uploaded ciphertext.
    pub ciphertext_sha256: String,
    pub ciphertext_size: u64,
    pub plaintext_size: u64,
    /// Protected presentation metadata (client-validated; the server never sees it).
    pub media_type: String,
    pub file_name: String,
}

/// The output of [`encrypt_attachment`].
#[derive(Debug)]
pub struct EncryptedAttachment {
    pub ciphertext: Vec<u8>,
    pub descriptor: AttachmentDescriptor,
}

fn chunk_aad(attachment_id: Uuid) -> Vec<u8> {
    let mut aad = b"cfmeet-attachment:v1:".to_vec();
    aad.extend_from_slice(attachment_id.hyphenated().to_string().as_bytes());
    aad
}

/// Size of the ciphertext of a `plaintext_size`-byte attachment (what the upload intent
/// declares).
pub fn ciphertext_size(plaintext_size: u64) -> u64 {
    let chunks = if plaintext_size == 0 {
        1
    } else {
        plaintext_size.div_ceil(CHUNK_SIZE as u64)
    };
    HEADER as u64 + plaintext_size + chunks * TAG as u64
}

/// Encrypts `plaintext` for the server-assigned `attachment_id` under a fresh key.
pub fn encrypt_attachment(
    attachment_id: Uuid,
    plaintext: &[u8],
    media_type: &str,
    file_name: &str,
) -> Result<EncryptedAttachment, AttachmentError> {
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(AttachmentError::TooLarge);
    }
    let key = AttachmentKey::generate();
    let mut prefix = [0u8; NONCE_PREFIX];
    OsRng.fill_bytes(&mut prefix);
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let mut encryptor = EncryptorBE32::from_aead(cipher, prefix.as_slice().into());
    let aad = chunk_aad(attachment_id);
    let mut out = Vec::with_capacity(ciphertext_size(plaintext.len() as u64) as usize);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&prefix);
    let mut chunks = plaintext.chunks(CHUNK_SIZE).peekable();
    if chunks.peek().is_none() {
        let last = encryptor
            .encrypt_last(aead::Payload {
                msg: &[],
                aad: &aad,
            })
            .map_err(|_| AttachmentError::Malformed)?;
        out.extend_from_slice(&last);
    } else {
        while let Some(chunk) = chunks.next() {
            let payload = aead::Payload {
                msg: chunk,
                aad: &aad,
            };
            if chunks.peek().is_some() {
                let c = encryptor
                    .encrypt_next(payload)
                    .map_err(|_| AttachmentError::Malformed)?;
                out.extend_from_slice(&c);
            } else {
                let c = encryptor
                    .encrypt_last(payload)
                    .map_err(|_| AttachmentError::Malformed)?;
                out.extend_from_slice(&c);
                break;
            }
        }
    }
    let descriptor = AttachmentDescriptor {
        format: ATTACHMENT_FORMAT_V1.to_string(),
        attachment_id,
        key,
        ciphertext_sha256: hex(&Sha256::digest(&out)),
        ciphertext_size: out.len() as u64,
        plaintext_size: plaintext.len() as u64,
        media_type: media_type.to_string(),
        file_name: file_name.to_string(),
    };
    Ok(EncryptedAttachment {
        ciphertext: out,
        descriptor,
    })
}

/// Verifies `ciphertext` against `descriptor` and decrypts it.
pub fn decrypt_attachment(
    descriptor: &AttachmentDescriptor,
    ciphertext: &[u8],
) -> Result<Vec<u8>, AttachmentError> {
    if descriptor.format != ATTACHMENT_FORMAT_V1 {
        return Err(AttachmentError::Malformed);
    }
    if hex(&Sha256::digest(ciphertext)) != descriptor.ciphertext_sha256
        || ciphertext.len() as u64 != descriptor.ciphertext_size
    {
        return Err(AttachmentError::DigestMismatch);
    }
    if ciphertext.len() < HEADER + TAG || &ciphertext[..MAGIC.len()] != MAGIC {
        return Err(AttachmentError::Malformed);
    }
    let prefix = &ciphertext[MAGIC.len()..HEADER];
    let cipher = XChaCha20Poly1305::new(descriptor.key.as_bytes().into());
    let mut decryptor = DecryptorBE32::from_aead(cipher, prefix.into());
    let aad = chunk_aad(descriptor.attachment_id);
    let body = &ciphertext[HEADER..];
    let mut out = Vec::with_capacity(descriptor.plaintext_size as usize);
    let mut chunks = body.chunks(CHUNK_SIZE + TAG).peekable();
    while let Some(chunk) = chunks.next() {
        let payload = aead::Payload {
            msg: chunk,
            aad: &aad,
        };
        if chunks.peek().is_some() {
            out.extend_from_slice(
                &decryptor
                    .decrypt_next(payload)
                    .map_err(|_| AttachmentError::Authentication)?,
            );
        } else {
            out.extend_from_slice(
                &decryptor
                    .decrypt_last(payload)
                    .map_err(|_| AttachmentError::Authentication)?,
            );
            break;
        }
    }
    if out.len() as u64 != descriptor.plaintext_size {
        return Err(AttachmentError::Authentication);
    }
    Ok(out)
}

impl AttachmentDescriptor {
    /// Canonical length-prefixed encoding (what the client places in the MLS message).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = b"CFAD1".to_vec();
        let mut field = |b: &[u8]| {
            out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            out.extend_from_slice(b);
        };
        field(self.format.as_bytes());
        field(self.attachment_id.as_bytes());
        field(self.key.as_bytes());
        field(self.ciphertext_sha256.as_bytes());
        field(&self.ciphertext_size.to_be_bytes());
        field(&self.plaintext_size.to_be_bytes());
        field(self.media_type.as_bytes());
        field(self.file_name.as_bytes());
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AttachmentError> {
        let rest = bytes
            .strip_prefix(b"CFAD1")
            .ok_or(AttachmentError::Malformed)?;
        let mut fields = Vec::new();
        let mut at = 0usize;
        while at < rest.len() {
            let len = u32::from_be_bytes(
                rest.get(at..at + 4)
                    .ok_or(AttachmentError::Malformed)?
                    .try_into()
                    .unwrap(),
            ) as usize;
            at += 4;
            fields.push(rest.get(at..at + len).ok_or(AttachmentError::Malformed)?);
            at += len;
        }
        if fields.len() != 8 {
            return Err(AttachmentError::Malformed);
        }
        let text = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| AttachmentError::Malformed);
        let u64_of = |b: &[u8]| -> Result<u64, AttachmentError> {
            Ok(u64::from_be_bytes(
                b.try_into().map_err(|_| AttachmentError::Malformed)?,
            ))
        };
        Ok(Self {
            format: text(fields[0])?,
            attachment_id: Uuid::from_slice(fields[1]).map_err(|_| AttachmentError::Malformed)?,
            key: AttachmentKey::from_bytes(
                fields[2]
                    .try_into()
                    .map_err(|_| AttachmentError::Malformed)?,
            ),
            ciphertext_sha256: text(fields[3])?,
            ciphertext_size: u64_of(fields[4])?,
            plaintext_size: u64_of(fields[5])?,
            media_type: text(fields[6])?,
            file_name: text(fields[7])?,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_at_every_chunk_boundary_and_sizes_are_predictable() {
        for len in [
            0,
            1,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            3 * CHUNK_SIZE + 7,
        ] {
            let id = Uuid::new_v4();
            let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let enc = encrypt_attachment(id, &plain, "image/png", "cat.png").unwrap();
            assert_eq!(
                enc.ciphertext.len() as u64,
                ciphertext_size(len as u64),
                "{len}"
            );
            let d = AttachmentDescriptor::from_bytes(&enc.descriptor.to_bytes()).unwrap();
            assert_eq!(d, enc.descriptor);
            assert_eq!(decrypt_attachment(&d, &enc.ciphertext).unwrap(), plain);
        }
    }

    #[test]
    fn the_ciphertext_reveals_nothing_and_resists_tampering() {
        let id = Uuid::new_v4();
        let plain = b"top secret scan of a passport ".repeat(5000);
        let enc = encrypt_attachment(id, &plain, "application/pdf", "passport.pdf").unwrap();
        assert!(!enc.ciphertext.windows(10).any(|w| w == b"top secret"));
        // Fresh key and nonce every time.
        let again = encrypt_attachment(id, &plain, "application/pdf", "passport.pdf").unwrap();
        assert_ne!(again.ciphertext, enc.ciphertext);
        assert_ne!(again.descriptor.key, enc.descriptor.key);
        // A flipped byte fails the digest; with a matching digest it fails AEAD.
        let mut forged = enc.ciphertext.clone();
        forged[HEADER + 3] ^= 1;
        assert_eq!(
            decrypt_attachment(&enc.descriptor, &forged),
            Err(AttachmentError::DigestMismatch)
        );
        let mut d = enc.descriptor.clone();
        d.ciphertext_sha256 = hex(&Sha256::digest(&forged));
        assert_eq!(
            decrypt_attachment(&d, &forged),
            Err(AttachmentError::Authentication)
        );
        // Truncation (dropping the last chunk) is detected.
        let cut =
            enc.ciphertext[..enc.ciphertext.len() - (plain.len() % CHUNK_SIZE + TAG)].to_vec();
        let mut d = enc.descriptor.clone();
        d.ciphertext_sha256 = hex(&Sha256::digest(&cut));
        d.ciphertext_size = cut.len() as u64;
        assert_eq!(
            decrypt_attachment(&d, &cut),
            Err(AttachmentError::Authentication)
        );
        // The same ciphertext presented as another attachment fails.
        let mut d = enc.descriptor.clone();
        d.attachment_id = Uuid::new_v4();
        assert_eq!(
            decrypt_attachment(&d, &enc.ciphertext),
            Err(AttachmentError::Authentication)
        );
        assert!(!format!("{:?}", enc.descriptor).contains(&format!("{:?}", enc.descriptor.key.0)));
    }
}
