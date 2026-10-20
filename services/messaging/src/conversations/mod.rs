//! The Conversation aggregate (Part V Workstream D).
//!
//! * [`ContextClass`] — the eight GENERIC context classes. A product's order, trip,
//!   consultation or case is only an opaque [`ContextRef`] in the application's own
//!   namespace: it confers no membership or authority and Messaging never reads a
//!   product's database.
//! * [`LifecycleState`] — server-authoritative states and [`LifecycleState::can_become`],
//!   enforced again by a database trigger. `EXPIRED` is set by the server only.
//! * [`policy`] — deterministic effective policy (platform defaults + application
//!   overrides), persisted as immutable content-addressed snapshots.
//! * [`service`] — creation (idempotent; direct-pair and context-reference
//!   deduplication; no silent profile fallback), reads, lifecycle changes with optimistic
//!   concurrency, expiry — each state change committed with its outbox event.
//! * [`sequence`] — the canonical message-sequence allocator and the server acceptance
//!   record that binds a server sequence / id to a client-encrypted MLS message.

pub mod access;
pub mod http;
pub mod policy;
pub mod sequence;
pub mod service;

use serde::{Deserialize, Serialize};

macro_rules! text_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $s)] $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(value: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == value)
            }
        }
    };
}

text_enum!(
    /// Generic conversation context classes.
    ContextClass {
        Direct = "DIRECT",
        Group = "GROUP",
        Meeting = "MEETING",
        Transaction = "TRANSACTION",
        Consultation = "CONSULTATION",
        Support = "SUPPORT",
        Enterprise = "ENTERPRISE",
        DeveloperDefined = "DEVELOPER_DEFINED",
    }
);

text_enum!(
    /// Conversation lifecycle.
    LifecycleState {
        Active = "ACTIVE",
        Locked = "LOCKED",
        Closed = "CLOSED",
        Archived = "ARCHIVED",
        Expired = "EXPIRED",
        Deleted = "DELETED",
    }
);

impl LifecycleState {
    /// The server-authoritative transition table (mirrored by the database trigger).
    pub fn can_become(&self, next: LifecycleState) -> bool {
        use LifecycleState::*;
        matches!(
            (self, next),
            (Active, Locked | Closed | Archived | Expired | Deleted)
                | (Locked, Active | Closed | Archived | Expired | Deleted)
                | (Closed, Active | Archived | Expired | Deleted)
                | (Archived, Active | Deleted)
                | (Expired, Archived | Deleted)
        )
    }

    /// Whether clients may REQUEST this state (EXPIRED is the server's alone).
    pub fn client_requestable(&self) -> bool {
        !matches!(self, LifecycleState::Expired)
    }

    /// Whether new messages are accepted in this state (enforced by Workstream F).
    pub fn accepts_messages(&self) -> bool {
        matches!(self, LifecycleState::Active)
    }

    /// States that still occupy a direct pair / context reference.
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            LifecycleState::Active
                | LifecycleState::Locked
                | LifecycleState::Closed
                | LifecycleState::Archived
        )
    }

    /// States the expiry applies to.
    pub fn expirable(&self) -> bool {
        matches!(
            self,
            LifecycleState::Active | LifecycleState::Locked | LifecycleState::Closed
        )
    }
}

/// An opaque, application-namespaced reference to an external context (an order, a
/// consultation, a case …). Bounded, safely encoded; never authority.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextRef {
    pub namespace: String,
    pub reference: String,
    #[serde(default = "one")]
    pub version: i32,
}

fn one() -> i32 {
    1
}

impl ContextRef {
    pub fn validate(&self) -> Result<(), &'static str> {
        let ns = self.namespace.as_bytes();
        let ns_ok = (1..=63).contains(&ns.len())
            && ns[0].is_ascii_lowercase()
            && ns
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        let reference_ok = (1..=256).contains(&self.reference.len())
            && self.reference.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'/' | b'-')
            });
        if !ns_ok {
            Err("context.namespace must be 1-63 characters of [a-z0-9-], starting with a letter")
        } else if !reference_ok {
            Err("context.reference must be 1-256 characters of [A-Za-z0-9._:/-]")
        } else if self.version < 1 {
            Err("context.version must be a positive integer")
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_generic_context_classes_exist() {
        assert_eq!(ContextClass::ALL.len(), 8);
        for c in ContextClass::ALL {
            assert_eq!(ContextClass::parse(c.as_str()), Some(*c));
        }
        assert_eq!(ContextClass::parse("ORDER"), None);
        assert_eq!(ContextClass::parse("direct"), None);
    }

    #[test]
    fn lifecycle_transitions_are_explicit() {
        use LifecycleState::*;
        assert!(Active.can_become(Locked) && Locked.can_become(Active));
        assert!(Closed.can_become(Active), "reopening is a transition");
        assert!(!Deleted.can_become(Active) && !Deleted.can_become(Archived));
        assert!(!Expired.can_become(Active), "expiry is never undone");
        assert!(!Archived.can_become(Locked));
        for s in LifecycleState::ALL {
            assert!(!s.can_become(*s), "{s:?} -> itself is not a transition");
        }
        assert!(!Expired.client_requestable());
        assert!(
            Active.accepts_messages() && !Locked.accepts_messages() && !Closed.accepts_messages()
        );
    }

    #[test]
    fn context_references_are_bounded_and_safely_encoded() {
        let ok = ContextRef {
            namespace: "orders".into(),
            reference: "ord_123/v2".into(),
            version: 1,
        };
        assert!(ok.validate().is_ok());
        for (ns, r) in [
            ("Orders", "x"),
            ("1orders", "x"),
            ("orders", ""),
            ("orders", "a b"),
            ("orders", "<script>"),
            ("orders", "'; DROP TABLE x; --"),
        ] {
            assert!(
                ContextRef {
                    namespace: ns.into(),
                    reference: r.into(),
                    version: 1
                }
                .validate()
                .is_err(),
                "{ns} {r}"
            );
        }
        assert!(ContextRef {
            namespace: "o".into(),
            reference: "x".repeat(257),
            version: 1
        }
        .validate()
        .is_err());
        assert!(ContextRef {
            namespace: "o".into(),
            reference: "x".into(),
            version: 0
        }
        .validate()
        .is_err());
    }
}
