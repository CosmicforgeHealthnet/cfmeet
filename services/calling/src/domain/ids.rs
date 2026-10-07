//! Typed identifiers. Every platform identifier is a server-generated UUID; distinct
//! types stop a tenant id from being passed where an application id is expected.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A fresh, server-generated identifier.
            pub(crate) fn generate() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Uuid {
                id.0
            }
        }
    };
}

typed_id!(
    /// Platform customer: the top-level security, ownership and usage boundary.
    TenantId
);
typed_id!(
    /// Product / application owned by exactly one tenant.
    ApplicationId
);
typed_id!(
    /// One SANDBOX / TEST / PRODUCTION environment of an application.
    EnvironmentId
);
typed_id!(
    /// Machine identity bound to one application environment.
    ServiceAccountId
);
typed_id!(
    /// API credential owned by one service account.
    CredentialId
);
typed_id!(
    /// Communication principal (participant / entity) inside one application environment.
    PrincipalId
);
