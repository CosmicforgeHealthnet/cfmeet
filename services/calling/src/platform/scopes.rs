//! Scope names and the authenticated scope set.
//!
//! The scope registry, application grants, environment restrictions and trust rules are
//! Platform Access's (ADR 0032). Calling receives each authenticated credential's
//! EFFECTIVE scopes from Platform Access and checks the scope an operation needs; a
//! request can never add a scope.

use std::collections::BTreeSet;

use serde::Serialize;

/// Well-known neutral CF Meet scope names.
pub mod names {
    /// Resolve communication principals inside the credential's application environment.
    pub const PRINCIPALS_READ: &str = "principals.read";
    /// Register / update communication principals inside the credential's environment.
    pub const PRINCIPALS_WRITE: &str = "principals.write";
    /// Read calls (reserved for the Calling API).
    pub const CALLS_READ: &str = "calls.read";
    /// Create and control calls (reserved for the Calling API).
    pub const CALLS_WRITE: &str = "calls.write";
    /// Private-network, low-latency event delivery (reserved; trusted).
    pub const EVENTS_PRIVATE_DELIVERY: &str = "events.private_delivery";
    /// Read Privacy Relay communication contexts of the credential's environment.
    pub const PRIVACY_CONTEXTS_READ: &str = "privacy.contexts.read";
    /// Create, update and change the lifecycle of communication contexts.
    pub const PRIVACY_CONTEXTS_WRITE: &str = "privacy.contexts.write";
    /// Read Privacy Relay sessions of the credential's environment.
    pub const PRIVACY_SESSIONS_READ: &str = "privacy.sessions.read";
    /// Create Privacy Relay sessions and change their lifecycle.
    pub const PRIVACY_SESSIONS_WRITE: &str = "privacy.sessions.write";
    /// Read protected contacts (masked) through the Contact Vault (Part D §31).
    pub const PRIVACY_CONTACTS_READ: &str = "privacy.contacts.read";
    /// Create / replace / disable protected contacts through the Contact Vault.
    pub const PRIVACY_CONTACTS_WRITE: &str = "privacy.contacts.write";
    /// Assert contact verification state (a trusted integration; §33).
    pub const PRIVACY_CONTACTS_VERIFY: &str = "privacy.contacts.verify";
    /// Read privacy-safe call evidence of Privacy Relay sessions (Part D §36).
    pub const PRIVACY_EVIDENCE_READ: &str = "privacy.evidence.read";
    /// Read enterprise calling configuration (Part F §19).
    pub const ENTERPRISE_CONFIG_READ: &str = "enterprise.config.read";
    /// Create / change / disable enterprise calling configuration.
    pub const ENTERPRISE_CONFIG_WRITE: &str = "enterprise.config.write";
    /// Place and control calls to enterprise destinations (extensions).
    pub const ENTERPRISE_CALLS_USE: &str = "enterprise.calls.use";
    /// Read voicemail messages of mailboxes the request is authorized for.
    pub const ENTERPRISE_VOICEMAIL_READ: &str = "enterprise.voicemail.read";
    /// Change read state, delete voicemail; configure mailboxes.
    pub const ENTERPRISE_VOICEMAIL_MANAGE: &str = "enterprise.voicemail.manage";
    /// Configure call-recording policies and start / stop recordings.
    pub const ENTERPRISE_RECORDING_MANAGE: &str = "enterprise.recording.manage";
    /// Read call-recording metadata and request authorized media access (audited).
    pub const ENTERPRISE_RECORDING_READ: &str = "enterprise.recording.read";
    /// Read customer webhook subscriptions and their delivery history (Part G §68).
    pub const WEBHOOKS_READ: &str = "webhooks.read";
    /// Create, change, delete customer webhook subscriptions, rotate their signing secret
    /// and replay deliveries (Part G §68).
    pub const WEBHOOKS_MANAGE: &str = "webhooks.manage";
    /// Read the application's usage facts and effective limits (Part G §69 / §70).
    pub const USAGE_READ: &str = "usage.read";
}

/// A scope name granted by the Platform Access scope registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Scope(String);

impl Scope {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Set of scopes held by an authenticated credential.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ScopeSet(BTreeSet<Scope>);

impl ScopeSet {
    pub fn contains(&self, name: &str) -> bool {
        self.0.iter().any(|s| s.as_str() == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scope> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.0.iter().map(|s| s.0.clone()).collect()
    }

    /// The effective scopes Platform Access returned for an authenticated credential
    /// (already validated there against the registry, the tenant classification, the
    /// application grant and its environment restriction). Never request data.
    pub(crate) fn from_authority(names: impl IntoIterator<Item = String>) -> Self {
        Self(names.into_iter().map(Scope).collect())
    }
}

