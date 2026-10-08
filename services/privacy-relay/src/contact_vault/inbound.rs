//! Telephony-only inbound caller protection (§35).
//!
//! An inbound public-network caller's presented number is as sensitive as any stored
//! contact. Once Privacy Relay has resolved the called relay number to a mapping (which
//! fixes the tenant / application / environment), Telephony hands the presented number
//! DIRECTLY to the Contact Vault, which canonicalizes it, fingerprints it, deduplicates
//! it in THAT scope (a known contact calling back is recognized as its existing
//! ContactReference) or encrypts a new UNVERIFIED contact, and answers only the
//! ContactReference. Calling never sees the number; logs and audit carry only the
//! ContactReference.

use serde::Deserialize;
use uuid::Uuid;

use super::{
    audit::{record_refusal, AccessEntry, ExecutionRefs},
    normalization::RawContactValue,
    service::canonicalize,
    ContactActor, ContactOperation, ContactVault,
};
use crate::error::RelayResult;

/// Purpose recorded in the access audit.
pub const PURPOSE: &str = "INBOUND_CALLER_PROTECTION";

/// `POST /internal/v1/relay-routing/protect-inbound-caller`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtectInboundRequest {
    /// The RESOLVED inbound resolution (same Telephony service, recent) whose scope the
    /// caller is protected in.
    pub inbound_resolution_id: Uuid,
    /// The caller's presented number, exactly as the provider presented it.
    pub telephone_number: RawContactValue,
}

impl std::fmt::Debug for ProtectInboundRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectInboundRequest")
            .field("inbound_resolution_id", &self.inbound_resolution_id)
            .finish_non_exhaustive()
    }
}

impl ContactVault {
    /// Protects an inbound caller's presented number in `actor`'s (mapping-derived)
    /// scope. `default_country` (the relay number's country) only applies to a number
    /// presented without its international prefix. Returns (created, ContactReference).
    pub async fn protect_inbound(
        &self,
        actor: &ContactActor,
        request: ProtectInboundRequest,
        default_country: Option<&str>,
        refs: ExecutionRefs,
    ) -> RelayResult<(bool, Uuid)> {
        let raw: RawContactValue = request.telephone_number;
        let entry = AccessEntry::new(ContactOperation::ProtectInbound, None)
            .purpose(PURPOSE)
            .refs(refs);
        let result = async {
            let keys = self.keys_for_write()?;
            let number = canonicalize(&raw, default_country)?;
            drop(raw);
            self.protect(keys, actor, &number, entry.clone()).await
        }
        .await;
        match result {
            Ok((created, row)) => {
                tracing::info!(contact_reference = %row.id, created, "inbound caller protected");
                Ok((created, row.id))
            }
            Err(e) => {
                record_refusal(&self.db, actor, entry, &e, self.now()).await;
                Err(e)
            }
        }
    }
}
