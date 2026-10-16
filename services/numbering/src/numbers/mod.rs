//! The number domain (Part IV Workstream D).
//!
//! A **Number Resource** is the canonical CF Meet identity of one public number; its id
//! never changes, whatever provider currently serves it. Everything else hangs off it:
//! provider bindings (internal), capabilities (with provenance and effective dates),
//! inventory observations (never an allocation), reservations (scoped to a trusted
//! environment), DCN assignments (who owns and routes the number, separate from the
//! resource), routing profiles (versioned, opaque targets) and append-only history.
//!
//! Every state change goes through this module's transitions inside one database
//! transaction that also writes the lifecycle / assignment history and the outbox event.
//! The database independently enforces the same edges, exclusivity and scope rules
//! (migration `m20261016_000002_number_domain`).

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $(
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

pub mod acquisitions;
pub mod assignments;
pub mod bindings;
pub mod capabilities;
pub mod catalog;
pub mod e164;
pub mod eligibility;
pub mod error;
pub mod inventory;
pub mod kyc;
pub mod lifecycle;
pub mod limitations;
pub mod ownership;
pub mod pools;
pub mod reservations;
pub mod resolution;
pub mod resources;
pub mod routing;
pub mod types;

use sea_orm::{DatabaseConnection, DbBackend, Statement};

pub use error::{DomainError, DomainResult};
pub use types::*;

use crate::domain::SharedClock;

/// The number domain over Numbering's own database.
#[derive(Clone)]
pub struct NumberDomain {
    db: DatabaseConnection,
    clock: SharedClock,
}

impl NumberDomain {
    pub fn new(db: DatabaseConnection, clock: SharedClock) -> Self {
        Self { db, clock }
    }

    pub fn db(&self) -> &DatabaseConnection {
        &self.db
    }

    pub fn clock(&self) -> &SharedClock {
        &self.clock
    }
}

pub(crate) fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}
