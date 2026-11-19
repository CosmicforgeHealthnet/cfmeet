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
    tls_codec::Deserialize as _, BasicCredential, ContentType, MlsMessageBodyIn, MlsMessageIn,
    ProtocolMessage, Sender, WireFormat,
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
    /// A `PublicMessage` commit whose sender is `new_member_commit`: an external commit,
    /// by which a device joins the group by itself (RFC 9420 §12.4.3.2). Its sender type
    /// is in the clear; the joiner's identity is verified by every member's client.
    pub new_member_commit: bool,
}

/// The public header of an encoded `GroupInfo` (RFC 9420 §12.4.3): its group and epoch,
/// and the device ids of the leaves of the ratchet tree it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfoHeader {
    pub group_id: Vec<u8>,
    pub epoch: u64,
    /// The CF Meet device ids of the tree's leaves (`None` without a ratchet tree
    /// extension). A device absent here is not in the group at this epoch: an external
    /// commit built from this GroupInfo encrypts nothing to it.
    pub member_devices: Option<Vec<uuid::Uuid>>,
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
    let new_member_commit = match &protocol {
        ProtocolMessage::PublicMessage(m) => {
            matches!(m.sender(), Sender::NewMemberCommit) && content == FramedContent::Commit
        }
        _ => false,
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
        new_member_commit,
    })
}

/// Reads the group id and epoch of an encoded `MLSMessage` carrying a `GroupInfo` (exact
/// decoding). Its signature is verified by the joining client against the signer's leaf.
pub fn inspect_group_info(encoded: &[u8]) -> Result<GroupInfoHeader, FramingError> {
    if encoded.len() > MAX_FRAMED_BYTES {
        return Err(FramingError::TooLarge);
    }
    let message =
        MlsMessageIn::tls_deserialize_exact(encoded).map_err(|_| FramingError::Malformed)?;
    match message.extract() {
        MlsMessageBodyIn::GroupInfo(info) => {
            let member_devices = match info.extensions().ratchet_tree() {
                Some(tree) => Some(
                    tree.ratchet_tree()
                        .leaves()
                        .map(|leaf| {
                            BasicCredential::try_from(leaf.credential().clone())
                                .ok()
                                .and_then(|c| {
                                    crate::binding::parse_device_credential_identity(c.identity())
                                })
                                .ok_or(FramingError::Malformed)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                None => None,
            };
            Ok(GroupInfoHeader {
                group_id: info.group_id().as_slice().to_vec(),
                epoch: info.epoch().as_u64(),
                member_devices,
            })
        }
        _ => Err(FramingError::Malformed),
    }
}

/// Whether `encoded` is a well-formed MLS `Welcome` message (structure only).
pub fn is_welcome(encoded: &[u8]) -> bool {
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
        assert_eq!(inspect_group_info(b"nope"), Err(FramingError::Malformed));
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
            let _ = inspect_group_info(&bytes);
        }
    }
}
