//! Platform vocabulary shared by every Calling module: typed identifiers, persisted
//! enumerations, the clock abstraction, request identifiers and slug validation.

/// String-backed enumeration persisted as PostgreSQL `TEXT` guarded by a `CHECK`
/// constraint. The wire / database value is the SCREAMING_SNAKE_CASE literal.
macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            sea_orm::EnumIter, sea_orm::DeriveActiveEnum,
            serde::Serialize, serde::Deserialize,
        )]
        #[sea_orm(rs_type = "String", db_type = "Text")]
        pub enum $name {
            $(
                #[sea_orm(string_value = $s)]
                #[serde(rename = $s)]
                $variant,
            )+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value { $($s => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

/// Server-generated UUID identifier with its own type, so one kind of id can never be
/// passed where another is expected.
macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// A fresh, server-generated identifier. (Platform identity ids are minted by
            /// Platform Access, ADR 0032; Calling mints only its own.)
            #[allow(dead_code)]
            pub(crate) fn generate() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            pub fn from_uuid(id: uuid::Uuid) -> Self {
                Self(id)
            }

            pub fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<$name> for uuid::Uuid {
            fn from(id: $name) -> uuid::Uuid {
                id.0
            }
        }
    };
}

pub mod clock;
pub mod ids;
pub mod kinds;
pub mod request_id;
pub mod slug;

pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use ids::{
    ApplicationId, CredentialId, EnvironmentId, PrincipalId, ServiceAccountId, TenantId,
};
pub use kinds::{
    ApplicationStatus, CredentialStatus, EnvironmentKind, EnvironmentStatus, PrincipalKind,
    PrincipalStatus, ServiceAccountStatus, TenantClassification, TenantStatus,
};
pub use request_id::{ClientCorrelationId, CorrelationId, RequestContext, RequestId};
pub use slug::Slug;
