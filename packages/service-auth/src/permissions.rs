//! Internal permission registry.
//!
//! Permissions are least-privilege, `<resource>.<action>` and TARGET-SERVICE
//! AUTHORITATIVE: only the receiving service's trust bundle grants them, never the
//! caller's token. Each permission names the single service that receives (enforces) it
//! and the issuers it may ever be granted to; a trust bundle that grants it elsewhere is
//! refused at startup.
//!
//! Privacy invariant: `protected-contact.resolve-for-execution` (resolving a protected
//! contact reference for execution of an authorized call leg) is received only by Privacy
//! Relay and grantable only to Telephony. Calling can never be granted plaintext-contact
//! resolution.
//!
//! There are no wildcard, `admin`, `all` or `root` permissions.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

use crate::identity::services;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum InternalPermission {
    /// Privacy Relay: authorize a communication.
    #[serde(rename = "communication.authorize")]
    CommunicationAuthorize,
    /// Privacy Relay: read the permitted communication context.
    #[serde(rename = "communication-context.read")]
    CommunicationContextRead,
    /// Privacy Relay: resolve a protected contact reference for execution of a specific,
    /// authorized call leg. Telephony only.
    #[serde(rename = "protected-contact.resolve-for-execution")]
    ProtectedContactResolveForExecution,
    /// Telephony: execute an authorized public-network call leg.
    #[serde(rename = "call-leg.execute")]
    CallLegExecute,
    /// Telephony: read the status of an authorized call leg.
    #[serde(rename = "call-leg-status.read")]
    CallLegStatusRead,
    /// Calling: receive tightly scoped operational callbacks.
    #[serde(rename = "operational-callback.deliver")]
    OperationalCallbackDeliver,
}

/// Registry entry: which service receives the permission and who may be granted it.
#[derive(Debug, Clone, Copy)]
pub struct PermissionDefinition {
    pub permission: InternalPermission,
    pub name: &'static str,
    pub receiver: &'static str,
    pub grantable_to: &'static [&'static str],
    /// Grants access to protected (privacy-sensitive) data.
    pub sensitive: bool,
    pub description: &'static str,
}

pub const REGISTRY: &[PermissionDefinition] = &[
    PermissionDefinition {
        permission: InternalPermission::CommunicationAuthorize,
        name: "communication.authorize",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Authorize a communication within a communication context",
    },
    PermissionDefinition {
        permission: InternalPermission::CommunicationContextRead,
        name: "communication-context.read",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING, services::TELEPHONY],
        sensitive: false,
        description: "Read the permitted communication context (opaque references only)",
    },
    PermissionDefinition {
        permission: InternalPermission::ProtectedContactResolveForExecution,
        name: "protected-contact.resolve-for-execution",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::TELEPHONY],
        sensitive: true,
        description:
            "Resolve a protected contact reference for execution of a specific authorized call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::CallLegExecute,
        name: "call-leg.execute",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Execute an authorized public-network call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::CallLegStatusRead,
        name: "call-leg-status.read",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Read the status of an authorized call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::OperationalCallbackDeliver,
        name: "operational-callback.deliver",
        receiver: services::CALLING,
        grantable_to: &[services::PRIVACY_RELAY, services::TELEPHONY],
        sensitive: false,
        description: "Deliver tightly scoped operational callbacks to Calling",
    },
];

/// Values that must never be used as an ordinary permission.
const WILDCARD_LIKE: &[&str] = &["*", "admin", "all", "root", "any", "superuser"];

impl InternalPermission {
    pub const ALL: &'static [InternalPermission] = &[
        InternalPermission::CommunicationAuthorize,
        InternalPermission::CommunicationContextRead,
        InternalPermission::ProtectedContactResolveForExecution,
        InternalPermission::CallLegExecute,
        InternalPermission::CallLegStatusRead,
        InternalPermission::OperationalCallbackDeliver,
    ];

    pub fn definition(&self) -> &'static PermissionDefinition {
        REGISTRY
            .iter()
            .find(|d| d.permission == *self)
            .expect("every permission has a registry entry")
    }

    pub fn as_str(&self) -> &'static str {
        self.definition().name
    }

    pub fn parse(value: &str) -> Option<Self> {
        REGISTRY
            .iter()
            .find(|d| d.name == value)
            .map(|d| d.permission)
    }

    /// Whether `value` looks like a wildcard / super-permission.
    pub fn is_wildcard_like(value: &str) -> bool {
        let lower = value.trim().to_ascii_lowercase();
        value.contains('*')
            || WILDCARD_LIKE.contains(&lower.as_str())
            || lower
                .split(['.', ':', '-'])
                .any(|part| WILDCARD_LIKE.contains(&part))
    }
}

impl fmt::Display for InternalPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Permissions granted to an authenticated caller by the receiver's policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet(BTreeSet<InternalPermission>);

impl PermissionSet {
    pub(crate) fn new(permissions: BTreeSet<InternalPermission>) -> Self {
        Self(permissions)
    }

    pub fn contains(&self, permission: InternalPermission) -> bool {
        self.0.contains(&permission)
    }

    pub fn iter(&self) -> impl Iterator<Item = &InternalPermission> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(InternalPermission::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_consistent() {
        assert_eq!(REGISTRY.len(), InternalPermission::ALL.len());
        for p in InternalPermission::ALL {
            let d = p.definition();
            assert_eq!(InternalPermission::parse(d.name), Some(*p));
            assert_eq!(serde_json::to_value(p).unwrap(), d.name);
            assert!(
                !d.grantable_to.contains(&d.receiver),
                "{} grantable to its own receiver",
                d.name
            );
            assert!(!InternalPermission::is_wildcard_like(d.name));
            let (resource, action) = d.name.split_once('.').unwrap();
            for part in [resource, action] {
                assert!(part
                    .split('-')
                    .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase())));
            }
        }
    }

    #[test]
    fn protected_contact_resolution_is_telephony_only() {
        let d = InternalPermission::ProtectedContactResolveForExecution.definition();
        assert_eq!(d.receiver, services::PRIVACY_RELAY);
        assert_eq!(d.grantable_to, &[services::TELEPHONY]);
        assert!(d.sensitive);
        // No sensitive permission is ever grantable to Calling.
        for d in REGISTRY.iter().filter(|d| d.sensitive) {
            assert!(!d.grantable_to.contains(&services::CALLING), "{}", d.name);
        }
    }

    #[test]
    fn wildcard_like_values_are_detected() {
        for bad in [
            "*",
            "admin",
            "ALL",
            "root",
            "calls.*",
            "admin.all",
            "*.read",
            "superuser",
        ] {
            assert!(InternalPermission::is_wildcard_like(bad), "{bad}");
            assert!(InternalPermission::parse(bad).is_none());
        }
    }
}
