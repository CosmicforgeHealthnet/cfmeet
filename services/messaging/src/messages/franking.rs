//! Message franking of PRIVATE_E2EE messages (S2): verifiable abuse reports without the
//! server ever reading E2EE content.
//!
//! The SENDER client draws a fresh random 32-byte franking key per message, puts the key
//! INSIDE the MLS plaintext it encrypts (so every recipient learns it on decryption), and
//! sends the commitment
//!
//! ```text
//! frankingCommitment = hex(HMAC-SHA256(key,
//!     "cfmeet-messaging-franking:v1" || 0x00 || conversationId (16 bytes)
//!     || clientMessageId (16 bytes) || senderPrincipalId (16 bytes) || plaintext (UTF-8)))
//! ```
//!
//! next to the ciphertext (`POST …/messages`, optional). The server stores it immutable with
//! the accepted message ([`store`]) and returns it with the message. A RECIPIENT client
//! should check, after decrypting, that the key and plaintext it got reproduce the
//! commitment (otherwise the sender framed something else). A reporter reveals the
//! plaintext and the key in the report disclosure; the moderator view recomputes the
//! commitment from the stored message's ids ([`verify`]): a match proves that THIS sender
//! sent THIS plaintext in THIS message (the server bound the commitment to the sender's
//! authenticated identity at acceptance). The server never sees a key or plaintext unless
//! a member chooses to report.

use hmac::{Hmac, Mac};
use sea_orm::ConnectionTrait;
use sha2::Sha256;
use uuid::Uuid;

use crate::{
    db,
    error::{Error, ErrorCode, Result},
};

pub const LABEL: &[u8] = b"cfmeet-messaging-franking:v1";
pub const KEY_BYTES: usize = 32;

fn mac(
    key: &[u8],
    conversation: Uuid,
    client_message: Uuid,
    sender: Uuid,
    text: &str,
) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(LABEL);
    m.update(&[0]);
    m.update(conversation.as_bytes());
    m.update(client_message.as_bytes());
    m.update(sender.as_bytes());
    m.update(text.as_bytes());
    m
}

/// The commitment a sender computes (hex).
pub fn commitment(
    key: &[u8],
    conversation: Uuid,
    client_message: Uuid,
    sender: Uuid,
    text: &str,
) -> String {
    hex::encode(
        mac(key, conversation, client_message, sender, text)
            .finalize()
            .into_bytes(),
    )
}

/// A submitted commitment: exactly 64 lower-case hex characters.
pub fn parse_commitment(value: &str) -> Result<String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(value.to_string())
    } else {
        Err(Error::validation(
            "frankingCommitment must be 64 lower-case hex characters (HMAC-SHA256)",
        ))
    }
}

/// Whether the revealed `key_b64` (unpadded base64url, 32 bytes) and `text` reproduce the
/// stored `commitment` (constant-time comparison).
pub fn verify(
    commitment: &str,
    key_b64: &str,
    conversation: Uuid,
    client_message: Uuid,
    sender: Uuid,
    text: &str,
) -> bool {
    let Ok(key) = super::decode_b64("frankingKey", key_b64, KEY_BYTES) else {
        return false;
    };
    let Ok(expected) = hex::decode(commitment) else {
        return false;
    };
    key.len() == KEY_BYTES
        && mac(&key, conversation, client_message, sender, text)
            .verify_slice(&expected)
            .is_ok()
}

/// Stores the commitment of a newly accepted message (same transaction as the message).
pub async fn store<C: ConnectionTrait>(
    db: &C,
    message_id: Uuid,
    conversation_id: Uuid,
    commitment: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    db::exec(
        db,
        "INSERT INTO messaging_message_frankings (message_id, conversation_id, commitment, created_at) \
         VALUES ($1, $2, $3, $4)",
        vec![message_id.into(), conversation_id.into(), commitment.into(), now.into()],
    )
    .await
    .map_err(|_| Error::code(ErrorCode::InternalError))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revealed_key_and_plaintext_verify_only_their_own_message() {
        let key = [7u8; 32];
        let (c, m, s) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let tag = commitment(&key, c, m, s, "hello");
        assert!(parse_commitment(&tag).is_ok());
        let k = super::super::b64(&key);
        assert!(verify(&tag, &k, c, m, s, "hello"));
        assert!(!verify(&tag, &k, c, m, s, "hello!"), "other plaintext");
        assert!(
            !verify(&tag, &k, c, m, Uuid::new_v4(), "hello"),
            "other sender"
        );
        assert!(
            !verify(&tag, &k, Uuid::new_v4(), m, s, "hello"),
            "other conversation"
        );
        assert!(
            !verify(&tag, &super::super::b64(&[8u8; 32]), c, m, s, "hello"),
            "other key"
        );
        assert!(
            !verify(&tag, &super::super::b64(&[7u8; 16]), c, m, s, "hello"),
            "short key"
        );
        assert!(parse_commitment(&tag.to_uppercase()).is_err());
        assert!(parse_commitment("abc").is_err());
    }
}
