//! Contact Vault (Part D §31–§33): the only place in CF Meet where a private telephone
//! number exists, and only as authenticated ciphertext.
//!
//! Boundary rules (enforced by this module's API and by `tests/contact_*`):
//!
//! * a raw number enters ONLY through the dedicated Contact Vault routes (`http`),
//!   authenticated by a short-lived Contact Vault access token minted by Calling — never
//!   through Calling, the internal API or any other body;
//! * it is canonicalized (`normalization`), encrypted with AES-256-GCM bound to its
//!   owner (`crypto`) under an externally managed, versioned key (`keyring`), and only
//!   the ciphertext, nonce and key VERSION are stored;
//! * every other service and every ordinary response sees an opaque ContactReference
//!   and a bounded masked display;
//! * every sensitive operation writes `contact_access_audit` (`audit`), success or not;
//! * plaintext leaves only through execution resolution (§33), to Telephony, for one
//!   authorized call leg.
//!
//! Nothing here is ever logged with a value: secret types are redacted, errors are fixed
//! codes, and request bodies are never logged.

pub mod audit;
pub mod crypto;
pub mod fingerprint;
pub mod http;
pub mod keyring;
pub mod model;
pub mod normalization;
pub mod repository;
pub mod service;

use std::sync::Arc;

use cf_service_auth::{ContactAccess, CorrelationId, RequestId, ServiceId};
use uuid::Uuid;

use crate::{
    clock::SharedClock,
    config::FeatureFlag,
    error::{ErrorCode, RelayError, RelayResult},
    scope::RelayScope,
};
use keyring::{ensure_independent, ContactKeyProvider, KeyringError};
use sea_orm::DatabaseConnection;

/// The two independent keyrings.
#[derive(Clone)]
pub struct VaultKeys {
    pub(crate) encryption: Arc<dyn ContactKeyProvider>,
    pub(crate) fingerprint: Arc<dyn ContactKeyProvider>,
}

impl VaultKeys {
    pub fn new(
        encryption: Arc<dyn ContactKeyProvider>,
        fingerprint: Arc<dyn ContactKeyProvider>,
    ) -> Result<Self, KeyringError> {
        ensure_independent(encryption.as_ref(), fingerprint.as_ref())?;
        Ok(Self {
            encryption,
            fingerprint,
        })
    }

    /// The same fingerprint keyring with another encryption keyring (rotation deploys).
    pub fn with_encryption(
        &self,
        encryption: Arc<dyn ContactKeyProvider>,
    ) -> Result<Self, KeyringError> {
        Self::new(encryption, self.fingerprint.clone())
    }

    /// The same encryption keyring with another fingerprint keyring.
    pub fn with_fingerprint(
        &self,
        fingerprint: Arc<dyn ContactKeyProvider>,
    ) -> Result<Self, KeyringError> {
        Self::new(self.encryption.clone(), fingerprint)
    }

    pub fn encryption(&self) -> &dyn ContactKeyProvider {
        self.encryption.as_ref()
    }

    pub fn fingerprint(&self) -> &dyn ContactKeyProvider {
        self.fingerprint.as_ref()
    }
}

/// The Contact Vault domain service.
#[derive(Clone)]
pub struct ContactVault {
    pub(crate) db: DatabaseConnection,
    pub(crate) clock: SharedClock,
    pub(crate) enabled: FeatureFlag,
    pub(crate) keys: Option<VaultKeys>,
}

impl ContactVault {
    /// `keys` = None: the vault is not configured; operations that need key material fail
    /// closed with CONTACT_VAULT_DISABLED (no plaintext fallback exists).
    pub fn new(
        db: DatabaseConnection,
        clock: SharedClock,
        enabled: FeatureFlag,
        keys: Option<VaultKeys>,
    ) -> Self {
        Self {
            db,
            clock,
            enabled,
            keys,
        }
    }

    pub fn enabled(&self) -> &FeatureFlag {
        &self.enabled
    }

    pub fn is_configured(&self) -> bool {
        self.keys.is_some()
    }

    /// Key material for an operation that ADDS or REVEALS contact data: requires the
    /// feature enabled and the keyrings configured.
    pub(crate) fn keys_for_write(&self) -> RelayResult<&VaultKeys> {
        if !self.enabled.enabled() {
            return Err(RelayError::Refused(ErrorCode::ContactVaultDisabled));
        }
        self.keys
            .as_ref()
            .ok_or(RelayError::Refused(ErrorCode::ContactVaultDisabled))
    }

    /// Key material for restrictive / maintenance work (re-encryption, fingerprint
    /// backfill): available while the feature flag is off.
    pub(crate) fn keys_any(&self) -> RelayResult<&VaultKeys> {
        self.keys
            .as_ref()
            .ok_or(RelayError::Refused(ErrorCode::ContactVaultDisabled))
    }
}

/// Who performs a Contact Vault operation (trusted inputs only; never deserialized).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactActorKind {
    /// A customer backend with a Contact Vault access token.
    DelegatedClient,
    /// An internal service (Calling, Telephony) with a service token.
    Service,
    /// Privacy Relay itself (workers, operator tooling).
    System,
}

impl ContactActorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContactActorKind::DelegatedClient => "DELEGATED_CLIENT",
            ContactActorKind::Service => "SERVICE",
            ContactActorKind::System => "SYSTEM",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContactActor {
    pub(crate) scope: RelayScope,
    pub(crate) kind: ContactActorKind,
    pub(crate) service: ServiceId,
    pub(crate) service_account_id: Option<Uuid>,
    pub(crate) credential_ref: Option<String>,
    pub(crate) request_id: RequestId,
    pub(crate) correlation_id: CorrelationId,
}

impl ContactActor {
    /// A customer request authenticated by a Contact Vault access token.
    pub fn delegated_client(
        access: &ContactAccess,
        request_id: RequestId,
        correlation_id: CorrelationId,
    ) -> Self {
        Self {
            scope: RelayScope::of_delegated(access.delegated()),
            kind: ContactActorKind::DelegatedClient,
            service: access.issuer().clone(),
            service_account_id: Some(access.delegated().initiating_service_account_id()),
            credential_ref: Some(access.token_id().as_str().chars().take(64).collect()),
            request_id,
            correlation_id,
        }
    }

    /// An internal service acting in a signed delegated scope.
    pub fn service(
        scope: RelayScope,
        service: ServiceId,
        service_account_id: Option<Uuid>,
        request_id: RequestId,
        correlation_id: CorrelationId,
    ) -> Self {
        Self {
            scope,
            kind: ContactActorKind::Service,
            service,
            service_account_id,
            credential_ref: None,
            request_id,
            correlation_id,
        }
    }

    /// Privacy Relay itself, for a row of `scope`.
    pub fn system(scope: RelayScope) -> Self {
        Self {
            scope,
            kind: ContactActorKind::System,
            service: crate::config::privacy_relay_service_id(),
            service_account_id: None,
            credential_ref: None,
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
        }
    }

    pub fn scope(&self) -> RelayScope {
        self.scope
    }
}

/// Audited Contact Vault operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactOperation {
    Create,
    Read,
    Update,
    Disable,
    Verify,
    Reencrypt,
    FingerprintBackfill,
    GrantForExecution,
    ResolveForExecution,
    ProtectInbound,
    ExpireVerification,
}

impl ContactOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContactOperation::Create => "CREATE",
            ContactOperation::Read => "READ",
            ContactOperation::Update => "UPDATE",
            ContactOperation::Disable => "DISABLE",
            ContactOperation::Verify => "VERIFY",
            ContactOperation::Reencrypt => "REENCRYPT",
            ContactOperation::FingerprintBackfill => "FINGERPRINT_BACKFILL",
            ContactOperation::GrantForExecution => "GRANT_FOR_EXECUTION",
            ContactOperation::ResolveForExecution => "RESOLVE_FOR_EXECUTION",
            ContactOperation::ProtectInbound => "PROTECT_INBOUND",
            ContactOperation::ExpireVerification => "EXPIRE_VERIFICATION",
        }
    }
}
