//! Migration history owned by the Calling service (`services/calling/`).
//!
//! This history is independent of the Meeting Service migrations: it never creates,
//! alters or references Meeting Service tables, and it records applied migrations in its
//! own `calling_seaql_migrations` table so it stays unambiguous even if an operator
//! points both services at the same PostgreSQL server.

pub use sea_orm_migration::prelude::*;

mod m20261007_000001_platform_consumers;
mod m20261007_000002_platform_credentials;
mod m20261007_000003_platform_principals;
mod m20261007_000004_platform_limits;
mod m20261007_000005_call_core;
mod m20261007_000006_call_participants_sessions;
mod m20261007_000007_call_routes_legs_attempts;
mod m20261007_000008_call_evidence_quality_usage;
mod m20261007_000009_call_outbox;
mod m20261007_000010_call_endpoints_and_invitations;
mod m20261007_000011_call_answer_arbitration;
mod m20261007_000012_call_media_permissions_and_bindings;
mod m20261007_000013_call_control_idempotency_and_transfers;
mod m20261007_000014_call_media_provider_events;
mod m20261008_000015_call_privacy_relay_evidence;
mod m20261008_000016_call_privacy_relay_index;
mod m20261008_000017_call_route_plans;
mod m20261008_000018_call_public_network;

/// Name of the table recording which Calling migrations have been applied.
pub const MIGRATION_TABLE: &str = "calling_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261007_000001_platform_consumers::Migration),
            Box::new(m20261007_000002_platform_credentials::Migration),
            Box::new(m20261007_000003_platform_principals::Migration),
            Box::new(m20261007_000004_platform_limits::Migration),
            Box::new(m20261007_000005_call_core::Migration),
            Box::new(m20261007_000006_call_participants_sessions::Migration),
            Box::new(m20261007_000007_call_routes_legs_attempts::Migration),
            Box::new(m20261007_000008_call_evidence_quality_usage::Migration),
            Box::new(m20261007_000009_call_outbox::Migration),
            Box::new(m20261007_000010_call_endpoints_and_invitations::Migration),
            Box::new(m20261007_000011_call_answer_arbitration::Migration),
            Box::new(m20261007_000012_call_media_permissions_and_bindings::Migration),
            Box::new(m20261007_000013_call_control_idempotency_and_transfers::Migration),
            Box::new(m20261007_000014_call_media_provider_events::Migration),
            Box::new(m20261008_000015_call_privacy_relay_evidence::Migration),
            Box::new(m20261008_000016_call_privacy_relay_index::Migration),
            Box::new(m20261008_000017_call_route_plans::Migration),
            Box::new(m20261008_000018_call_public_network::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}
