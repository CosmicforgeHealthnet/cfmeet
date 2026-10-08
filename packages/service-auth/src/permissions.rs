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
//! Communication authority: Calling (the authenticated platform ingress) may be granted
//! the communication-context / relay-session proxy permissions and `communication.authorize`
//! by Privacy Relay; Privacy Relay may be granted `communication-authority.enforce` by
//! Calling, which can only ever terminate protected calls, never grant authority.
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
    /// Privacy Relay: create, update and change the lifecycle of communication contexts
    /// on behalf of the delegated application (authenticated platform ingress).
    #[serde(rename = "communication-context.write")]
    CommunicationContextWrite,
    /// Privacy Relay: read privacy relay sessions of the delegated application.
    #[serde(rename = "relay-session.read")]
    RelaySessionRead,
    /// Privacy Relay: create privacy relay sessions and change their lifecycle on behalf
    /// of the delegated application.
    #[serde(rename = "relay-session.write")]
    RelaySessionWrite,
    /// Calling: enforce a restrictive communication-authority change (revocation,
    /// participant removal, suspension, expiry) on protected calls. Restrictive only.
    #[serde(rename = "communication-authority.enforce")]
    CommunicationAuthorityEnforce,
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
    PermissionDefinition {
        permission: InternalPermission::CommunicationContextWrite,
        name: "communication-context.write",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Create, update and change the lifecycle of communication contexts for the delegated application",
    },
    PermissionDefinition {
        permission: InternalPermission::RelaySessionRead,
        name: "relay-session.read",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Read privacy relay sessions of the delegated application (opaque references only)",
    },
    PermissionDefinition {
        permission: InternalPermission::RelaySessionWrite,
        name: "relay-session.write",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Create privacy relay sessions and change their lifecycle for the delegated application",
    },
    PermissionDefinition {
        permission: InternalPermission::CommunicationAuthorityEnforce,
        name: "communication-authority.enforce",
        receiver: services::CALLING,
        grantable_to: &[services::PRIVACY_RELAY],
        sensitive: false,
        description: "Enforce a restrictive communication-authority change on protected calls (terminate only)",
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
        InternalPermission::CommunicationContextWrite,
        InternalPermission::RelaySessionRead,
        InternalPermission::RelaySessionWrite,
        InternalPermission::CommunicationAuthorityEnforce,
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
    fn communication_authority_flows_are_least_privilege() {
        use InternalPermission::*;
        // Calling -> Privacy Relay: the platform ingress proxy and call authorization.
        for p in [
            CommunicationAuthorize,
            CommunicationContextWrite,
            RelaySessionRead,
            RelaySessionWrite,
        ] {
            let d = p.definition();
            assert_eq!(d.receiver, services::PRIVACY_RELAY, "{}", d.name);
            assert_eq!(d.grantable_to, &[services::CALLING], "{}", d.name);
        }
        // Privacy Relay -> Calling: restrictive enforcement only.
        let d = CommunicationAuthorityEnforce.definition();
        assert_eq!(d.receiver, services::CALLING);
        assert_eq!(d.grantable_to, &[services::PRIVACY_RELAY]);
        assert!(!d.sensitive);
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
