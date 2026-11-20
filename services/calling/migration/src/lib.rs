//! Migration history owned by the Calling service (`services/calling/`).
//!
//! This history is independent of the Meeting Service migrations: it never creates,
//! alters or references Meeting Service tables, and it records applied migrations in its
//! own `calling_seaql_migrations` table so it stays unambiguous even if an operator
//! points both services at the same PostgreSQL server.

pub use sea_orm_migration::prelude::*;

mod gap;
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
mod m20261201_000048_enterprise_audit_acting_principal;
mod m20261202_000049_call_kind_upgrade;
mod m20261202_000050_call_screen_share;
mod m20261202_000051_call_add_participants;

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
            Box::new(m20261201_000048_enterprise_audit_acting_principal::Migration),
            Box::new(m20261202_000049_call_kind_upgrade::Migration),
            Box::new(m20261202_000050_call_screen_share::Migration),
            Box::new(m20261202_000051_call_add_participants::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(MIGRATION_TABLE).into_iden()
    }
}

/// Name of the table recording which gap-closure migrations have been applied.
pub const GAP_CLOSURE_MIGRATION_TABLE: &str = "calling_gap_closure_seaql_migrations";

/// The gap-closure migrations: a SEPARATE, additive history (its own table) applied by
/// `calling-migration up` after [`Migrator`]. The main history fails closed on versions
/// it does not know (an older binary refuses a newer schema), so new additive changes
/// live here: the previous image keeps starting against a database that has them (it
/// never reads this table), while this image requires them ([`pending_migrations`]).
/// Every migration here is additive and backward compatible with the previous image.
pub struct GapClosureMigrator;

#[async_trait::async_trait]
impl MigratorTrait for GapClosureMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(gap::m20261203_000052_call_host_controls::Migration),
            Box::new(gap::m20261204_000053_call_history_visibility::Migration),
            Box::new(gap::m20261205_000054_direct_call_routing::Migration),
            Box::new(gap::m20261206_000055_queue_wrap_up_hold_music::Migration),
            Box::new(gap::m20261206_000056_call_consultations::Migration),
        ]
    }

    fn migration_table_name() -> DynIden {
        Alias::new(GAP_CLOSURE_MIGRATION_TABLE).into_iden()
    }
}

/// The migrations of [`Migrator`] and of [`GapClosureMigrator`] not yet recorded in their
/// tables — READ-ONLY.
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
    let main: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let gap: Vec<String> = GapClosureMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let kind_offer: Vec<String> = CallKindOfferMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let mut pending = pending_in(db, MIGRATION_TABLE, main).await?;
    pending.extend(pending_in(db, GAP_CLOSURE_MIGRATION_TABLE, gap).await?);
    pending.extend(pending_in(db, CALL_KIND_OFFER_MIGRATION_TABLE, kind_offer).await?);
    Ok(pending)
}

async fn pending_in<C>(
    db: &C,
    table: &str,
    known: Vec<String>,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!("SELECT to_regclass('{table}') IS NOT NULL AS present"),
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
            format!("SELECT version FROM {table}"),
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

mod m20261117_000001_notification_publications;

/// Notification Service roadmap N7: Calling's outbox of publications to the Notification
/// Service (`calling_notification_publications`). A THIRD separate history for the same
/// rollback-safety reason as the Communications one: the previous image fails closed on a
/// migration of [`MIGRATION_TABLE`] it does not know and checks the Communications history
/// exactly, so neither may gain this table. Never merge the histories.
pub struct NotificationPublicationsMigrator;

/// Where [`NotificationPublicationsMigrator`] records its migrations.
pub const NOTIFICATION_PUBLICATIONS_MIGRATION_TABLE: &str =
    "calling_notification_publications_seaql_migrations";

#[async_trait::async_trait]
impl MigratorTrait for NotificationPublicationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            m20261117_000001_notification_publications::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(NOTIFICATION_PUBLICATIONS_MIGRATION_TABLE).into_iden()
    }
}

/// Read-only readiness of the notification-publications history: every migration of
/// [`NotificationPublicationsMigrator`] is applied and nothing else is recorded. Required
/// only while `CF_NOTIFICATIONS_BASE_URL` is configured (only then is the table used).
pub async fn notification_publications_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let present = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT to_regclass('public.{NOTIFICATION_PUBLICATIONS_MIGRATION_TABLE}') IS NOT NULL AS present"
            ),
        ))
        .await?
        .ok_or_else(|| sea_orm_migration::sea_orm::DbErr::Custom("readiness unavailable".into()))?
        .try_get::<bool>("", "present")?;
    if !present {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT version FROM {NOTIFICATION_PUBLICATIONS_MIGRATION_TABLE}"),
        ))
        .await?;
    let known: std::collections::BTreeSet<_> = NotificationPublicationsMigrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(known == actual)
}

mod kind_offer;

/// Where [`CallKindOfferMigrator`] records its migrations.
pub const CALL_KIND_OFFER_MIGRATION_TABLE: &str = "calling_call_kind_offer_seaql_migrations";

/// CF2 (a voice call switched to video while it still rings): its OWN history, for the
/// same rollback-safety reason as [`NotificationPublicationsMigrator`]. The previous image
/// fails closed on a migration of [`MIGRATION_TABLE`] or [`GAP_CLOSURE_MIGRATION_TABLE`] it
/// does not know, and never reads this table, so it keeps starting (and reporting ready)
/// after a rollback; this image requires it ([`pending_migrations`]). Never merge it.
pub struct CallKindOfferMigrator;

#[async_trait::async_trait]
impl MigratorTrait for CallKindOfferMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            kind_offer::m20261207_000057_call_kind_offer::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(CALL_KIND_OFFER_MIGRATION_TABLE).into_iden()
    }
}

/// `calling-migration up`: the main history, then the notification-publications, the
/// gap-closure and the call-kind-offer ones (the Communications history keeps its own
/// binary). All additive.
pub async fn up_all(
    db: &sea_orm_migration::sea_orm::DatabaseConnection,
) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
    Migrator::up(db, None).await?;
    NotificationPublicationsMigrator::up(db, None).await?;
    GapClosureMigrator::up(db, None).await?;
    CallKindOfferMigrator::up(db, None).await
}
