//! Platform vocabulary shared by every Calling module: typed identifiers, persisted
//! enumerations, the clock abstraction, request identifiers, redacted secret containers
//! and slug validation.

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

pub mod clock;
pub mod ids;
pub mod kinds;
pub mod request_id;
pub mod secret;
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
pub use secret::IssuedSecret;
pub use slug::Slug;
