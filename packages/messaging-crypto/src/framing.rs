//! Public MLS framing inspection (server-safe; Part V E/F, RFC 9420 §6).
//!
//! An MLS Delivery Service must order handshake and application messages per group and
//! epoch. RFC 9420 puts exactly the fields it needs in the CLEAR framing of every
//! `PrivateMessage` / `PublicMessage`: `group_id`, `epoch` and `content_type`
//! (§6.3.1). [`inspect`] reads those fields — it never decrypts, holds no key and cannot
//! see the sender of a `PrivateMessage` or any content. The backend uses it to refuse a
//! ciphertext submitted for another group, a stale / future epoch, or an application
//! message posing as a commit (and vice versa).

use openmls::prelude::{
    tls_codec::Deserialize as _, ContentType, MlsMessageIn, ProtocolMessage, WireFormat,
};

/// Upper bound of one inspected MLS message (an application message is bounded by the
/// service; a commit carrying an UpdatePath for a large group is larger).
pub const MAX_FRAMED_BYTES: usize = 1024 * 1024;

/// What the clear framing says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramedContent {
    Application,
    Proposal,
    Commit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramedWireFormat {
    /// Encrypted content and sender (`mls_private_message`).
    Private,
    /// Signed, unencrypted handshake (`mls_public_message`).
    Public,
}

/// The public framing of one MLS protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framing {
    pub group_id: Vec<u8>,
    pub epoch: u64,
    pub content: FramedContent,
    pub wire_format: FramedWireFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FramingError {
    #[error("the MLS message is too large")]
    TooLarge,
    #[error("not a well-formed MLS protocol message")]
    Malformed,
}

/// Reads the clear framing of one encoded `MLSMessage` (exact decoding, no trailing
/// bytes). Welcome / GroupInfo / KeyPackage messages are not protocol messages and are
/// refused as [`FramingError::Malformed`].
pub fn inspect(encoded: &[u8]) -> Result<Framing, FramingError> {
    if encoded.len() > MAX_FRAMED_BYTES {
        return Err(FramingError::TooLarge);
    }
    let message =
        MlsMessageIn::tls_deserialize_exact(encoded).map_err(|_| FramingError::Malformed)?;
    let protocol: ProtocolMessage = message
        .try_into_protocol_message()
        .map_err(|_| FramingError::Malformed)?;
    let content = match protocol.content_type() {
        ContentType::Application => FramedContent::Application,
        ContentType::Proposal => FramedContent::Proposal,
        ContentType::Commit => FramedContent::Commit,
    };
    let wire_format = match protocol.wire_format() {
        WireFormat::PrivateMessage => FramedWireFormat::Private,
        WireFormat::PublicMessage => FramedWireFormat::Public,
        _ => return Err(FramingError::Malformed),
    };
    Ok(Framing {
        group_id: protocol.group_id().as_slice().to_vec(),
        epoch: protocol.epoch().as_u64(),
        content,
        wire_format,
    })
}

/// Whether `encoded` is a well-formed MLS `Welcome` message (structure only).
pub fn is_welcome(encoded: &[u8]) -> bool {
    use openmls::prelude::MlsMessageBodyIn;
    encoded.len() <= MAX_FRAMED_BYTES
        && matches!(
            MlsMessageIn::tls_deserialize_exact(encoded).map(|m| m.extract()),
            Ok(MlsMessageBodyIn::Welcome(_))
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert_eq!(inspect(&[]), Err(FramingError::Malformed));
        assert_eq!(inspect(b"hello"), Err(FramingError::Malformed));
        assert_eq!(
            inspect(&vec![0u8; MAX_FRAMED_BYTES + 1]),
            Err(FramingError::TooLarge)
        );
        assert!(!is_welcome(b"nope"));
        let mut x: u64 = 0x1234_5678_9abc_def1;
        for len in 0..300usize {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            let _ = inspect(&bytes);
        }
    }
}
