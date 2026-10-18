//! Canonical message ordering foundation (Part V D.11).
//!
//! MLS application messages are encrypted on the client BEFORE the server assigns its
//! canonical sequence number or message id, so the client cannot authenticate those
//! server fields. The binding is therefore split:
//!
//! 1. **Client-authenticated data** — what the client knows before encrypting, carried in
//!    the MLS `authenticated_data` of the `PrivateMessage`
//!    ([`cf_messaging_crypto::binding::application_message_aad`], Appendix H.1): security
//!    profile, protocol and cipher suite, tenant / application / environment, conversation
//!    id, a client-chosen stable message id and the sender device. A ciphertext moved to
//!    another tenant, application, environment, conversation or profile, replayed under
//!    another id or attributed to another device fails MLS authentication on every
//!    recipient.
//! 2. **Server acceptance record** — [`AcceptanceRecord`]: the server binds the canonical
//!    sequence and server message id it assigns to the immutable SHA-256 digest of the
//!    exact ciphertext (plus every client-authenticated field, the scope and the profile /
//!    protocol / suite) and stores / returns the
//!    record digest. Recipients can check that the sequence they were given belongs to
//!    the ciphertext they decrypted; the server cannot reorder or substitute without the
//!    record changing.
//!
//! Sequences come from [`allocate_sequence`]: a transactional per-conversation counter
//! (row lock on its own table; gap-free per committed transaction; never Redis-only).
//! Its capacity is a later certification item. Persisting acceptance records is
//! Workstream F; the formats and the allocator are fixed and tested now.

use cf_messaging_crypto::{binding, profile};
use chrono::{DateTime, Utc};
use sea_orm::ConnectionTrait;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db,
    domain::profile::{OpaqueCiphertext, TransportMetadata},
    error::{Error, ErrorCode, Result},
};

/// Domain separation of the acceptance-record digest.
pub const ACCEPTANCE_LABEL: &[u8] = b"cfmeet-messaging-acceptance:v2";

/// Allocates the next canonical sequence number of `conversation_id` inside the caller's
/// transaction. Concurrent allocations serialize on the conversation's counter row; a
/// rolled-back transaction releases its number.
pub async fn allocate_sequence<C: ConnectionTrait>(db: &C, conversation_id: Uuid) -> Result<i64> {
    let row = db
        .query_one(db::stmt(
            "UPDATE messaging_conversation_sequences SET next_sequence = next_sequence + 1 \
             WHERE conversation_id = $1 RETURNING next_sequence - 1",
            vec![conversation_id.into()],
        ))
        .await?
        .ok_or(Error::code(ErrorCode::NotFound))?;
    Ok(row.try_get_by_index::<i64>(0)?)
}

/// Where a message is accepted: the authenticated authorization scope (from the request
/// context, never from the ciphertext) and the conversation with its MLS group id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcceptanceTarget {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub mls_group_id: [u8; 32],
}

/// The server's acceptance of one encrypted application message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceRecord {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub mls_group_id: [u8; 32],
    pub client_message_id: Uuid,
    pub sender_device_id: Uuid,
    pub epoch: u64,
    pub ciphertext_digest: [u8; 32],
    pub server_sequence: i64,
    pub server_message_id: Uuid,
    pub accepted_at: DateTime<Utc>,
}

impl AcceptanceRecord {
    pub fn new(
        target: AcceptanceTarget,
        metadata: &TransportMetadata,
        ciphertext: &OpaqueCiphertext,
        server_sequence: i64,
        server_message_id: Uuid,
        accepted_at: DateTime<Utc>,
    ) -> Self {
        Self {
            tenant_id: target.tenant_id,
            application_id: target.application_id,
            environment_id: target.environment_id,
            conversation_id: target.conversation_id,
            mls_group_id: target.mls_group_id,
            client_message_id: metadata.client_message_id,
            sender_device_id: metadata.sender_device_id,
            epoch: metadata.epoch,
            ciphertext_digest: ciphertext.digest(),
            server_sequence,
            server_message_id,
            accepted_at,
        }
    }

    /// SHA-256 over a fixed-layout, length-prefixed encoding (no ambiguity, no JSON).
    pub fn digest(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        let mut field = |bytes: &[u8]| {
            h.update((bytes.len() as u32).to_be_bytes());
            h.update(bytes);
        };
        field(ACCEPTANCE_LABEL);
        field(profile::SECURITY_PROFILE.as_bytes());
        field(profile::PROTOCOL_ID.as_bytes());
        field(profile::CIPHERSUITE_NAME.as_bytes());
        field(self.tenant_id.as_bytes());
        field(self.application_id.as_bytes());
        field(self.environment_id.as_bytes());
        field(self.conversation_id.as_bytes());
        field(&self.mls_group_id);
        field(self.client_message_id.as_bytes());
        field(self.sender_device_id.as_bytes());
        field(&self.epoch.to_be_bytes());
        field(&self.ciphertext_digest);
        field(&self.server_sequence.to_be_bytes());
        field(self.server_message_id.as_bytes());
        field(&self.accepted_at.timestamp_micros().to_be_bytes());
        h.finalize().into()
    }

    /// The client-authenticated binding this record accepted: the MLS AAD of the
    /// ciphertext must be exactly [`binding::application_message_aad`] of it.
    pub fn client_binding(&self) -> binding::ApplicationMessageBinding {
        binding::ApplicationMessageBinding {
            tenant_id: self.tenant_id,
            application_id: self.application_id,
            environment_id: self.environment_id,
            conversation_id: self.conversation_id,
            client_message_id: self.client_message_id,
            sender_device_id: self.sender_device_id,
        }
    }

    /// Whether this record accepts exactly `ciphertext`.
    pub fn covers(&self, ciphertext: &OpaqueCiphertext) -> bool {
        self.ciphertext_digest == ciphertext.digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(ct: &OpaqueCiphertext) -> AcceptanceRecord {
        let meta = TransportMetadata {
            client_message_id: Uuid::from_u128(1),
            sender_device_id: Uuid::from_u128(2),
            epoch: 7,
        };
        AcceptanceRecord::new(
            AcceptanceTarget {
                tenant_id: Uuid::from_u128(10),
                application_id: Uuid::from_u128(11),
                environment_id: Uuid::from_u128(12),
                conversation_id: Uuid::from_u128(3),
                mls_group_id: [9; 32],
            },
            &meta,
            ct,
            42,
            Uuid::from_u128(4),
            DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        )
    }

    #[test]
    fn the_acceptance_digest_binds_every_field() {
        let ct = OpaqueCiphertext::new(vec![1, 2, 3, 4]).unwrap();
        let base = record(&ct);
        let d = base.digest();
        assert_eq!(d, record(&ct).digest(), "deterministic");
        let mut variants = Vec::new();
        let mut r = base.clone();
        r.server_sequence = 43;
        variants.push(r);
        let mut r = base.clone();
        r.server_message_id = Uuid::from_u128(5);
        variants.push(r);
        let mut r = base.clone();
        r.ciphertext_digest = OpaqueCiphertext::new(vec![1, 2, 3, 5]).unwrap().digest();
        variants.push(r);
        let mut r = base.clone();
        r.conversation_id = Uuid::from_u128(6);
        variants.push(r);
        let mut r = base.clone();
        r.sender_device_id = Uuid::from_u128(7);
        variants.push(r);
        let mut r = base.clone();
        r.epoch = 8;
        variants.push(r);
        let mut r = base.clone();
        r.mls_group_id = [8; 32];
        variants.push(r);
        let mut r = base.clone();
        r.accepted_at += chrono::Duration::microseconds(1);
        variants.push(r);
        let mut r = base.clone();
        r.tenant_id = Uuid::from_u128(20);
        variants.push(r);
        let mut r = base.clone();
        r.application_id = Uuid::from_u128(21);
        variants.push(r);
        let mut r = base.clone();
        r.environment_id = Uuid::from_u128(22);
        variants.push(r);
        let mut r = base.clone();
        r.conversation_id = Uuid::from_u128(23);
        variants.push(r);
        let mut r = base.clone();
        r.client_message_id = Uuid::from_u128(24);
        variants.push(r);
        let mut r = base.clone();
        r.sender_device_id = Uuid::from_u128(25);
        variants.push(r);
        let mut r = base.clone();
        r.ciphertext_digest = [0; 32];
        variants.push(r);
        for v in variants {
            assert_ne!(v.digest(), d, "{v:?}");
        }
        assert!(base.covers(&ct));
        assert!(!base.covers(&OpaqueCiphertext::new(vec![1, 2, 3]).unwrap()));
    }

    #[test]
    fn client_aad_needs_no_server_assigned_field() {
        // The AAD is computable before encryption from client-known values only, and the
        // record names exactly the binding the client authenticated.
        let ct = OpaqueCiphertext::new(vec![1, 2, 3, 4]).unwrap();
        let r = record(&ct);
        let aad = binding::application_message_aad(&r.client_binding());
        let text = String::from_utf8(aad.clone()).unwrap();
        assert!(text.starts_with("cfmeet-messaging-aad:v2:PRIVATE_E2EE:MLS_1_0:"));
        assert!(!text.contains(&r.server_message_id.hyphenated().to_string()));
        assert_eq!(
            binding::parse_application_message_aad(&aad),
            Some(r.client_binding())
        );
    }

    #[test]
    fn the_crypto_profile_names_the_service_profile() {
        assert_eq!(
            profile::SECURITY_PROFILE,
            crate::domain::SecurityProfile::PrivateE2ee.as_str()
        );
    }
}
