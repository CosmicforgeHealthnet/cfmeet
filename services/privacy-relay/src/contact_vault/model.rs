//! Stored contact rows and their SAFE representation (contract
//! `cfmeet.privacy-relay.contact` `ProtectedContact`).
//!
//! The safe view is the only shape a contact ever leaves the vault in (outside execution
//! resolution): reference, masked display, country, capabilities, effective
//! verification, status, version and timestamps. Ciphertext, nonce, key versions and
//! fingerprints are never part of it.

use chrono::{DateTime, SecondsFormat, Utc};
use sea_orm::FromQueryResult;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{crypto::Sealed, keyring::KeyVersion};
use crate::{
    error::{ErrorCode, RelayError, RelayResult},
    scope::RelayScope,
};

/// Columns of [`ContactRow`].
pub const CONTACT_COLUMNS: &str =
    "id, tenant_id, application_id, environment_id, version, ciphertext, nonce, \
    encryption_key_version, masked_display, country_iso2, voice_capability, verification_state, \
    verification_expires_at, verification_updated_at, disabled_at, erased_at, created_at, updated_at";

#[derive(FromQueryResult, Clone)]
pub struct ContactRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub version: i64,
    /// NULL once the contact has been ERASED (retention minimization, §80).
    pub ciphertext: Option<Vec<u8>>,
    pub nonce: Option<Vec<u8>>,
    pub encryption_key_version: Option<String>,
    pub masked_display: String,
    pub country_iso2: Option<String>,
    pub voice_capability: String,
    pub verification_state: String,
    pub verification_expires_at: Option<DateTime<Utc>>,
    pub verification_updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
    /// Set once the value was erased; such a row is an opaque, disabled reference.
    pub erased_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for ContactRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContactRow")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("verification_state", &self.verification_state)
            .finish_non_exhaustive()
    }
}

/// Verification states (§33). INVALID and BLOCKED are both terminal-for-use states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationState {
    Unverified,
    Pending,
    Verified,
    Expired,
    Invalid,
    Blocked,
}

impl VerificationState {
    pub const ALL: [VerificationState; 6] = [
        VerificationState::Unverified,
        VerificationState::Pending,
        VerificationState::Verified,
        VerificationState::Expired,
        VerificationState::Invalid,
        VerificationState::Blocked,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            VerificationState::Unverified => "UNVERIFIED",
            VerificationState::Pending => "PENDING",
            VerificationState::Verified => "VERIFIED",
            VerificationState::Expired => "EXPIRED",
            VerificationState::Invalid => "INVALID",
            VerificationState::Blocked => "BLOCKED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }
}

impl ContactRow {
    pub fn scope(&self) -> RelayScope {
        RelayScope::from_row(self.tenant_id, self.application_id, self.environment_id)
    }

    /// The stored ciphertext; an erased contact has none (refused with the erased code).
    pub fn sealed(&self) -> RelayResult<Sealed> {
        let (Some(ciphertext), Some(nonce), Some(version)) = (
            self.ciphertext.as_ref(),
            self.nonce.as_ref(),
            self.encryption_key_version.as_deref(),
        ) else {
            return Err(super::erased());
        };
        Ok(Sealed {
            ciphertext: ciphertext.clone(),
            nonce: nonce.clone(),
            key_version: KeyVersion::parse(version)
                .ok_or(RelayError::Refused(ErrorCode::InternalError))?,
        })
    }

    pub fn is_erased(&self) -> bool {
        self.erased_at.is_some()
    }

    pub fn stored_state(&self) -> RelayResult<VerificationState> {
        VerificationState::parse(&self.verification_state)
            .ok_or(RelayError::Refused(ErrorCode::InternalError))
    }

    /// Verification as of `now`: a VERIFIED contact whose validity has passed is EXPIRED
    /// immediately, whether or not the expiry worker has materialized it.
    pub fn effective_state(&self, now: DateTime<Utc>) -> RelayResult<VerificationState> {
        let stored = self.stored_state()?;
        Ok(match (stored, self.verification_expires_at) {
            (VerificationState::Verified, Some(until)) if now >= until => {
                VerificationState::Expired
            }
            (s, _) => s,
        })
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }

    /// The safe view (contract `ProtectedContact`). An ERASED contact renders as a
    /// DISABLED reference whose masked display carries no digits (the contract has no
    /// separate ERASED status yet; `disabledAt` and the digit-free display tell the two
    /// apart).
    pub fn view(&self, now: DateTime<Utc>) -> RelayResult<Value> {
        let state = self.effective_state(now)?;
        Ok(json!({
            "contactReference": self.id,
            "maskedDisplay": self.masked_display,
            "country": self.country_iso2,
            "capabilities": { "voice": self.voice_capability },
            "verificationState": state.as_str(),
            "verificationExpiresAt": self.verification_expires_at.map(iso),
            "status": if self.is_disabled() { "DISABLED" } else { "ACTIVE" },
            "version": self.version,
            "createdAt": iso(self.created_at),
            "updatedAt": iso(self.updated_at),
            "disabledAt": self.disabled_at.map(iso),
        }))
    }
}

pub fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}
