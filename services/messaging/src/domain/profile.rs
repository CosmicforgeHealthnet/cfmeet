//! Security profiles and the content boundary (Part V §2, Appendix H).
//!
//! * [`SecurityProfile`] is persisted with every conversation and is never inferred from
//!   a client flag.
//! * [`OpaqueCiphertext`] is the ONLY content type a PRIVATE_E2EE path can carry: bytes the
//!   server routes and stores but never interprets. It has no text accessor, no `Display`,
//!   its `Debug` shows the length only, and nothing converts it into [`ManagedContent`].
//! * [`ManagedContent`] exists only for MANAGED conversations (server-processable by
//!   design) and can only be built through [`MessagePayload::accept`] for that profile.
//! * [`TransportMetadata`] — bounded, validated protocol metadata kept apart from content.
//!
//! The message send engine is Workstream F; these types fix the boundary it must use.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The security profile of a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SecurityProfile {
    /// Client-side MLS encryption; the server holds no decryption key.
    #[serde(rename = "PRIVATE_E2EE")]
    PrivateE2ee,
    /// Server-processable messaging under organizational governance. Never E2EE.
    #[serde(rename = "MANAGED")]
    Managed,
}

impl SecurityProfile {
    pub const ALL: [SecurityProfile; 2] = [SecurityProfile::PrivateE2ee, SecurityProfile::Managed];

    pub fn as_str(&self) -> &'static str {
        match self {
            SecurityProfile::PrivateE2ee => "PRIVATE_E2EE",
            SecurityProfile::Managed => "MANAGED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == value)
    }

    /// Whether the backend may ever process message content of this profile.
    pub fn server_may_process_content(&self) -> bool {
        matches!(self, SecurityProfile::Managed)
    }

    /// The protocol identifier persisted with conversations of this profile.
    pub fn protocol(&self) -> &'static str {
        match self {
            SecurityProfile::PrivateE2ee => cf_messaging_crypto::profile::PROTOCOL_ID,
            SecurityProfile::Managed => "CFMEET_MANAGED_V1",
        }
    }
}

/// Upper bound of one encrypted application message (MLS `PrivateMessage`).
pub const MAX_CIPHERTEXT_BYTES: usize = 256 * 1024;
/// Upper bound of one MANAGED text message (bytes, UTF-8).
pub const MAX_MANAGED_TEXT_BYTES: usize = 16 * 1024;

/// Encrypted content the server never interprets.
#[derive(Clone, PartialEq, Eq)]
pub struct OpaqueCiphertext(Vec<u8>);

impl OpaqueCiphertext {
    pub fn new(bytes: Vec<u8>) -> Result<Self, ContentError> {
        if bytes.is_empty() || bytes.len() > MAX_CIPHERTEXT_BYTES {
            return Err(ContentError::CiphertextSize);
        }
        Ok(Self(bytes))
    }

    /// The bytes as stored / relayed (never decoded as text).
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// SHA-256 of the ciphertext: what the server's acceptance record binds.
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(&self.0).into()
    }
}

impl fmt::Debug for OpaqueCiphertext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpaqueCiphertext({} bytes)", self.0.len())
    }
}

/// Server-processable content of a MANAGED conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedContent {
    text: String,
}

impl ManagedContent {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// What a client submitted, before the profile decides what it may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmittedContent {
    Ciphertext(Vec<u8>),
    ManagedText(String),
}

/// Content admitted for a conversation's profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessagePayload {
    E2ee(OpaqueCiphertext),
    Managed(ManagedContent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContentError {
    #[error("a PRIVATE_E2EE conversation accepts MLS ciphertext only")]
    PlaintextInE2ee,
    #[error("a MANAGED conversation does not accept E2EE ciphertext")]
    CiphertextInManaged,
    #[error("ciphertext size out of bounds")]
    CiphertextSize,
    #[error("managed text is empty, too long or contains control characters")]
    ManagedText,
}

impl MessagePayload {
    /// The only way to admit content: the conversation's PERSISTED profile decides.
    pub fn accept(
        profile: SecurityProfile,
        submitted: SubmittedContent,
    ) -> Result<Self, ContentError> {
        match (profile, submitted) {
            (SecurityProfile::PrivateE2ee, SubmittedContent::Ciphertext(bytes)) => {
                Ok(MessagePayload::E2ee(OpaqueCiphertext::new(bytes)?))
            }
            (SecurityProfile::PrivateE2ee, SubmittedContent::ManagedText(_)) => {
                Err(ContentError::PlaintextInE2ee)
            }
            (SecurityProfile::Managed, SubmittedContent::ManagedText(text)) => {
                let ok = !text.trim().is_empty()
                    && text.len() <= MAX_MANAGED_TEXT_BYTES
                    && !text
                        .chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\t');
                if ok {
                    Ok(MessagePayload::Managed(ManagedContent { text }))
                } else {
                    Err(ContentError::ManagedText)
                }
            }
            (SecurityProfile::Managed, SubmittedContent::Ciphertext(_)) => {
                Err(ContentError::CiphertextInManaged)
            }
        }
    }

    pub fn profile(&self) -> SecurityProfile {
        match self {
            MessagePayload::E2ee(_) => SecurityProfile::PrivateE2ee,
            MessagePayload::Managed(_) => SecurityProfile::Managed,
        }
    }
}

/// Bounded transport metadata of an E2EE application message, separate from content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransportMetadata {
    /// Client-chosen, stable id of the message (idempotency; bound into MLS AAD).
    pub client_message_id: Uuid,
    /// The sending device.
    pub sender_device_id: Uuid,
    /// The MLS epoch the client encrypted in.
    pub epoch: u64,
}

impl TransportMetadata {
    /// Epochs beyond 2^53 are refused (JSON-safe integers).
    pub fn validate(&self) -> Result<(), ContentError> {
        if self.epoch > (1u64 << 53)
            || self.client_message_id.is_nil()
            || self.sender_device_id.is_nil()
        {
            Err(ContentError::CiphertextSize)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_persisted_profile_decides_what_content_is_admissible() {
        let ct = vec![0x00, 0x01, 0x00, 0x02, 0xff];
        let e2ee = MessagePayload::accept(
            SecurityProfile::PrivateE2ee,
            SubmittedContent::Ciphertext(ct.clone()),
        )
        .unwrap();
        assert_eq!(e2ee.profile(), SecurityProfile::PrivateE2ee);
        assert_eq!(
            MessagePayload::accept(
                SecurityProfile::PrivateE2ee,
                SubmittedContent::ManagedText("hello".into())
            ),
            Err(ContentError::PlaintextInE2ee)
        );
        assert_eq!(
            MessagePayload::accept(SecurityProfile::Managed, SubmittedContent::Ciphertext(ct)),
            Err(ContentError::CiphertextInManaged)
        );
        let managed = MessagePayload::accept(
            SecurityProfile::Managed,
            SubmittedContent::ManagedText("hello".into()),
        )
        .unwrap();
        assert_eq!(managed.profile(), SecurityProfile::Managed);
        assert!(MessagePayload::accept(
            SecurityProfile::Managed,
            SubmittedContent::ManagedText("\u{0007}".into())
        )
        .is_err());
        assert!(MessagePayload::accept(
            SecurityProfile::PrivateE2ee,
            SubmittedContent::Ciphertext(vec![])
        )
        .is_err());
        assert!(OpaqueCiphertext::new(vec![0; MAX_CIPHERTEXT_BYTES + 1]).is_err());
    }

    #[test]
    fn ciphertext_never_prints_its_bytes() {
        let ct = OpaqueCiphertext::new(b"secret-looking-bytes".to_vec()).unwrap();
        let shown = format!("{ct:?}");
        assert_eq!(shown, "OpaqueCiphertext(20 bytes)");
        assert_eq!(ct.digest().len(), 32);
    }

    #[test]
    fn only_managed_content_is_server_processable() {
        assert!(!SecurityProfile::PrivateE2ee.server_may_process_content());
        assert!(SecurityProfile::Managed.server_may_process_content());
        assert_eq!(SecurityProfile::PrivateE2ee.protocol(), "MLS_1_0");
        for p in SecurityProfile::ALL {
            assert_eq!(SecurityProfile::parse(p.as_str()), Some(p));
        }
        assert_eq!(SecurityProfile::parse("private_e2ee"), None);
        assert_eq!(SecurityProfile::parse("E2EE"), None);
    }
}
