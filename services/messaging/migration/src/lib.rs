//! Migration history owned by the Messaging service (`services/messaging/`).
//!
//! Messaging (Part V) owns conversations, Messaging devices and their PUBLIC MLS material,
//! the key-transparency log, idempotency records and its outbox. Its history is recorded in
//! its own `messaging_seaql_migrations` table and is independent of every other service:
//! no table references another service's database (principals, tenants, applications and
//! environments are opaque Platform Access ids). No column may hold an E2EE private key, a
//! group / epoch secret or a plaintext E2EE message body (repository guard
//! `E2EE_SECRET_IN_SCHEMA`, migration tests).

pub use sea_orm_migration::prelude::*;

mod m20261101_000001_messaging_foundation;
mod m20261102_000002_conversations;
mod m20261105_000003_membership_messages;
mod m20261106_000004_realtime;
mod m20261107_000005_attachments;
mod m20261108_000006_legal_holds;
mod m20261109_000007_moderation_compliance;
mod m20261110_000008_integrations;
mod m20261111_000009_context_authority;
mod m20261112_000010_sms_bridge;
mod m20261113_000011_engagement_usage;
mod m20261114_000012_webhook_relay;
mod m20261115_000013_operations;
mod m20261116_000014_kt_witnesses;
mod m20261117_000015_history_transfers;
mod m20261118_000016_attachment_derivatives;
mod m20261119_000017_attachment_inspection;
mod m20261120_000018_derivative_quota_accounting;
mod m20261121_000019_derivative_reservations;
mod m20261122_000020_message_revisions;
mod m20261123_000021_received_cursors;
mod m20261124_000022_backups_and_switches;
mod m20261125_000023_managed_envelope_keys;
mod m20261126_000024_conversation_retention_grace_pins;
mod m20261127_000025_usage_jobs_incidents;
mod m20261128_000026_conversation_successors;
mod m20261129_000027_member_roles_invitations;
mod m20261130_000028_privacy_controls;
mod m20261201_000029_public_event_destinations;
mod m20261202_000030_session_devices_inbox;

/// Name of the table recording which Messaging migrations have been applied.
pub const MIGRATION_TABLE: &str = "messaging_seaql_migrations";

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261101_000001_messaging_foundation::Migration),
            Box::new(m20261102_000002_conversations::Migration),
            Box::new(m20261105_000003_membership_messages::Migration),
            Box::new(m20261106_000004_realtime::Migration),
            Box::new(m20261107_000005_attachments::Migration),
            Box::new(m20261108_000006_legal_holds::Migration),
            Box::new(m20261109_000007_moderation_compliance::Migration),
            Box::new(m20261110_000008_integrations::Migration),
            Box::new(m20261111_000009_context_authority::Migration),
            Box::new(m20261112_000010_sms_bridge::Migration),
            Box::new(m20261113_000011_engagement_usage::Migration),
            Box::new(m20261114_000012_webhook_relay::Migration),
            Box::new(m20261115_000013_operations::Migration),
            Box::new(m20261116_000014_kt_witnesses::Migration),
            Box::new(m20261117_000015_history_transfers::Migration),
            Box::new(m20261118_000016_attachment_derivatives::Migration),
            Box::new(m20261119_000017_attachment_inspection::Migration),
            Box::new(m20261120_000018_derivative_quota_accounting::Migration),
            Box::new(m20261121_000019_derivative_reservations::Migration),
            Box::new(m20261122_000020_message_revisions::Migration),
            Box::new(m20261123_000021_received_cursors::Migration),
            Box::new(m20261124_000022_backups_and_switches::Migration),
            Box::new(m20261125_000023_managed_envelope_keys::Migration),
            Box::new(m20261126_000024_conversation_retention_grace_pins::Migration),
            Box::new(m20261127_000025_usage_jobs_incidents::Migration),
            Box::new(m20261128_000026_conversation_successors::Migration),
            Box::new(m20261129_000027_member_roles_invitations::Migration),
            Box::new(m20261130_000028_privacy_controls::Migration),
            Box::new(m20261201_000029_public_event_destinations::Migration),
            Box::new(m20261202_000030_session_devices_inbox::Migration),
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
        Alias::new("messaging_communications_migrations").into_iden()
    }
}

/// Read-only readiness for the independently versioned additive integration.
pub async fn communications_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT to_regclass('public.messaging_communications_migrations') IS NOT NULL AS present".to_owned())).await?.ok_or_else(||sea_orm_migration::sea_orm::DbErr::Custom("readiness unavailable".into()))?.try_get::<bool>("","present")?;
    if !exists {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM messaging_communications_migrations".to_owned(),
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
