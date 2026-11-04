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
mod m20261008_000019_call_inbound_owners;
mod m20261009_000020_enterprise_foundation;
mod m20261009_000021_enterprise_extensions;
mod m20261009_000022_enterprise_ring_groups;
mod m20261009_000023_enterprise_queues;
mod m20261009_000024_enterprise_ivr;
mod m20261009_000025_enterprise_sip_devices;
mod m20261009_000026_enterprise_hours_forwarding;
mod m20261009_000027_voicemail;
mod m20261009_000028_call_recording;
mod m20261009_000029_call_history;
mod m20261009_000030_enterprise_operations;
mod m20261010_000031_platform_events_webhooks;
mod m20261010_000032_usage_and_limits;
mod m20261010_000033_service_account_and_principal_limits;
mod m20261010_000034_platform_operator_audit;
mod m20261010_000035_calling_block_rules;
mod m20261010_000036_platform_compliance_eligibility;
mod m20261010_000037_calling_abuse_reports;
mod m20261015_000038_platform_operator_audit_actions;
mod m20261015_000040_webhook_secret_key_ids;
mod m20261015_000041_calling_orphan_objects;
mod m20261015_000042_calling_retention;
mod m20261015_000043_calling_operational_safeguards;
mod m20261016_000044_platform_access_projection;
mod m20261025_000045_sms_webhook_producer;
mod m20261113_000046_messaging_webhook_producer;
mod m20261201_000047_public_event_destinations;

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
            Box::new(m20261008_000019_call_inbound_owners::Migration),
            Box::new(m20261009_000020_enterprise_foundation::Migration),
            Box::new(m20261009_000021_enterprise_extensions::Migration),
            Box::new(m20261009_000022_enterprise_ring_groups::Migration),
            Box::new(m20261009_000023_enterprise_queues::Migration),
            Box::new(m20261009_000024_enterprise_ivr::Migration),
            Box::new(m20261009_000025_enterprise_sip_devices::Migration),
            Box::new(m20261009_000026_enterprise_hours_forwarding::Migration),
            Box::new(m20261009_000027_voicemail::Migration),
            Box::new(m20261009_000028_call_recording::Migration),
            Box::new(m20261009_000029_call_history::Migration),
            Box::new(m20261009_000030_enterprise_operations::Migration),
            Box::new(m20261010_000031_platform_events_webhooks::Migration),
            Box::new(m20261010_000032_usage_and_limits::Migration),
            Box::new(m20261010_000033_service_account_and_principal_limits::Migration),
            Box::new(m20261010_000034_platform_operator_audit::Migration),
            Box::new(m20261010_000035_calling_block_rules::Migration),
            Box::new(m20261010_000036_platform_compliance_eligibility::Migration),
            Box::new(m20261010_000037_calling_abuse_reports::Migration),
            Box::new(m20261015_000038_platform_operator_audit_actions::Migration),
            Box::new(m20261015_000040_webhook_secret_key_ids::Migration),
            Box::new(m20261015_000041_calling_orphan_objects::Migration),
            Box::new(m20261015_000042_calling_retention::Migration),
            Box::new(m20261015_000043_calling_operational_safeguards::Migration),
            Box::new(m20261016_000044_platform_access_projection::Migration),
            Box::new(m20261025_000045_sms_webhook_producer::Migration),
            Box::new(m20261113_000046_messaging_webhook_producer::Migration),
            Box::new(m20261201_000047_public_event_destinations::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

/// The migrations of [`Migrator`] not yet recorded in [`MIGRATION_TABLE`] — READ-ONLY.
///
/// `MigratorTrait::get_pending_migrations` first (re)creates the migration table with DDL,
/// which the least-privilege runtime role
/// (`infra/deployment/postgres/provision-service-database.sh`: no DDL) is refused
/// (sqlstate 42501) even when the table exists — the service could neither start nor
/// report ready under its own role. This only `SELECT`s.
pub async fn pending_migrations<C>(db: &C) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!("SELECT to_regclass('{MIGRATION_TABLE}') IS NOT NULL AS present"),
        ))
        .await?
        .map(|r| r.try_get::<bool>("", "present"))
        .transpose()?
        .unwrap_or(false);
    if !present {
        return Ok(known);
    }
    let applied = db
        .query_all(Statement::from_string(
            backend,
            format!("SELECT version FROM {MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    // Same fail-closed rule as `get_pending_migrations`: a migration applied to the
    // database but unknown to this binary (an older binary on a newer schema) is an error.
    let mut unknown: Vec<&String> = applied.iter().filter(|v| !known.contains(v)).collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            unknown
                .iter()
                .map(|v| format!("Migration file of version '{v}' is missing, this migration has been applied but its file is missing"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

mod communications;
pub struct CommunicationsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for CommunicationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(communications::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("calling_communications_migrations").into_iden()
    }
}

/// Read-only readiness for the independently versioned additive integration.
pub async fn communications_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('public.calling_communications_migrations') IS NOT NULL AS present"
                .to_owned(),
        ))
        .await?
        .ok_or_else(|| sea_orm_migration::sea_orm::DbErr::Custom("readiness unavailable".into()))?
        .try_get::<bool>("", "present")?;
    if !exists {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM calling_communications_migrations".to_owned(),
        ))
        .await?;
    let known: std::collections::BTreeSet<_> = CommunicationsMigrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(known == actual)
}
