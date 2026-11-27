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
mod m20261203_000031_receipt_coalescing_timeline;
mod m20261204_000032_application_baselines;
mod m20261205_000033_client_session_binding;
mod m20261206_000034_forwarding_links_announcements_franking;
mod m20261207_000035_organization_moderation_team_channels;
mod m20261208_000036_partner_session_devices;
mod m20261209_000037_app_messages;
mod m20261210_000038_mls_group_infos;
mod m20261211_000039_member_notifications;
mod m20261212_000040_inbox_and_chat_controls;
mod m20261213_000041_admin_operations;
mod m20261214_000042_owner_delivered_exports;
mod m20261215_000043_admin_bridge_events;
mod m20261216_000044_chat_organization_polls;

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
            Box::new(m20261203_000031_receipt_coalescing_timeline::Migration),
            Box::new(m20261204_000032_application_baselines::Migration),
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
    let mut pending: Vec<String> = known.into_iter().filter(|m| !applied.contains(m)).collect();
    pending.extend(gap_closure_pending(db).await?);
    pending.extend(mls_join_pending(db).await?);
    pending.extend(member_notifications_pending(db).await?);
    pending.extend(chat_controls_pending(db).await?);
    pending.extend(admin_operations_pending(db).await?);
    pending.extend(chat_organization_pending(db).await?);
    Ok(pending)
}

/// Name of the table recording the MLS self-join history ([`MlsJoinMigrator`]).
pub const MLS_JOIN_MIGRATION_TABLE: &str = "messaging_mls_join_seaql_migrations";

/// MLS self-join (GroupInfo storage for external commits): a SEPARATE, additive history
/// applied by `messaging-migration up` after [`GapClosureMigrator`]. The previous image
/// fails closed on any gap-closure migration it does not know, so appending there would
/// stop it after a rollback; it never reads this table. Never merge the two. This image
/// requires it: [`pending_migrations`] includes it.
pub struct MlsJoinMigrator;
#[async_trait::async_trait]
impl MigratorTrait for MlsJoinMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(self::m20261210_000038_mls_group_infos::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(MLS_JOIN_MIGRATION_TABLE).into_iden()
    }
}

/// The MLS self-join migrations not yet applied (read-only; fails closed on unknown ones).
pub async fn mls_join_pending<C>(db: &C) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = MlsJoinMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!("SELECT to_regclass('{MLS_JOIN_MIGRATION_TABLE}') IS NOT NULL AS present"),
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
            format!("SELECT version FROM {MLS_JOIN_MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "an MLS self-join migration is applied but unknown to this binary".into(),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

/// Name of the table recording the member-notification history ([`MemberNotificationsMigrator`]).
pub const MEMBER_NOTIFICATIONS_MIGRATION_TABLE: &str =
    "messaging_member_notifications_seaql_migrations";

/// "Added to a group" notices for the Notification Service: a SEPARATE, additive history
/// applied by `messaging-migration up` after [`MlsJoinMigrator`]. The previous image fails
/// closed on unknown migrations of the histories it knows, so appending to one of them
/// would stop it after a rollback; it never reads this table. This image requires it:
/// [`pending_migrations`] includes it.
pub struct MemberNotificationsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for MemberNotificationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            self::m20261211_000039_member_notifications::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(MEMBER_NOTIFICATIONS_MIGRATION_TABLE).into_iden()
    }
}

/// The member-notification migrations not yet applied (read-only; fails closed on unknown
/// ones).
pub async fn member_notifications_pending<C>(
    db: &C,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = MemberNotificationsMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!(
                "SELECT to_regclass('{MEMBER_NOTIFICATIONS_MIGRATION_TABLE}') IS NOT NULL AS present"
            ),
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
            format!("SELECT version FROM {MEMBER_NOTIFICATIONS_MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "a member-notification migration is applied but unknown to this binary".into(),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

/// Name of the table recording the chat-controls history ([`ChatControlsMigrator`]).
pub const CHAT_CONTROLS_MIGRATION_TABLE: &str = "messaging_chat_controls_seaql_migrations";

/// Chat controls (archive / pin, cached send decisions, disappearing messages that can be
/// turned off): a SEPARATE, additive history applied by `messaging-migration up` after
/// [`MlsJoinMigrator`]. The previous image fails closed on unknown migrations of the
/// histories it knows, so appending to one of them would stop it after a rollback; it
/// never reads this table. This image requires it: [`pending_migrations`] includes it.
pub struct ChatControlsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for ChatControlsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            self::m20261212_000040_inbox_and_chat_controls::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(CHAT_CONTROLS_MIGRATION_TABLE).into_iden()
    }
}

/// The chat-controls migrations not yet applied (read-only; fails closed on unknown ones).
pub async fn chat_controls_pending<C>(
    db: &C,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = ChatControlsMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!("SELECT to_regclass('{CHAT_CONTROLS_MIGRATION_TABLE}') IS NOT NULL AS present"),
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
            format!("SELECT version FROM {CHAT_CONTROLS_MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "a chat-controls migration is applied but unknown to this binary".into(),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

/// Name of the table recording the admin-operations history ([`AdminOperationsMigrator`]).
pub const ADMIN_OPERATIONS_MIGRATION_TABLE: &str = "messaging_admin_operations_seaql_migrations";

/// HQ admin bridge commands (ADR 0068: the operations ledger and the report / legal-hold
/// row versions): a SEPARATE, additive history applied by `messaging-migration up` after
/// [`ChatControlsMigrator`]. The previous image fails closed on unknown migrations of the
/// histories it knows, so appending to one of them would stop it after a rollback; it
/// never reads this table. This image requires it: [`pending_migrations`] includes it.
pub struct AdminOperationsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for AdminOperationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(self::m20261213_000041_admin_operations::Migration),
            Box::new(self::m20261214_000042_owner_delivered_exports::Migration),
            Box::new(self::m20261215_000043_admin_bridge_events::Migration),
        ]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(ADMIN_OPERATIONS_MIGRATION_TABLE).into_iden()
    }
}

/// The admin-operations migrations not yet applied (read-only; fails closed on unknown
/// ones).
pub async fn admin_operations_pending<C>(
    db: &C,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = AdminOperationsMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!(
                "SELECT to_regclass('{ADMIN_OPERATIONS_MIGRATION_TABLE}') IS NOT NULL AS present"
            ),
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
            format!("SELECT version FROM {ADMIN_OPERATIONS_MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "an admin-operations migration is applied but unknown to this binary".into(),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

/// Name of the table recording the chat-organization history ([`ChatOrganizationMigrator`]).
pub const CHAT_ORGANIZATION_MIGRATION_TABLE: &str = "messaging_chat_organization_seaql_migrations";

/// Chat organization (favourites, custom chat lists), MANAGED polls and group
/// descriptions: a SEPARATE, additive history applied by `messaging-migration up` after
/// [`AdminOperationsMigrator`]. The previous image fails closed on unknown migrations of
/// the histories it knows, so appending to one of them would stop it after a rollback; it
/// never reads this table. This image requires it: [`pending_migrations`] includes it.
pub struct ChatOrganizationMigrator;
#[async_trait::async_trait]
impl MigratorTrait for ChatOrganizationMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            self::m20261216_000044_chat_organization_polls::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(CHAT_ORGANIZATION_MIGRATION_TABLE).into_iden()
    }
}

/// The chat-organization migrations not yet applied (read-only; fails closed on unknown
/// ones).
pub async fn chat_organization_pending<C>(
    db: &C,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = ChatOrganizationMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!(
                "SELECT to_regclass('{CHAT_ORGANIZATION_MIGRATION_TABLE}') IS NOT NULL AS present"
            ),
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
            format!("SELECT version FROM {CHAT_ORGANIZATION_MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "a chat-organization migration is applied but unknown to this binary".into(),
        ));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

/// Name of the table recording the backend gap-closure history ([`GapClosureMigrator`]).
pub const GAP_CLOSURE_MIGRATION_TABLE: &str = "messaging_gap_closure_seaql_migrations";

/// Backend gap closure (A11 client-session binding, ...): an independently versioned,
/// additive history applied by `messaging-migration up` after [`Migrator`], so a previous
/// Messaging image (whose [`Migrator`] fails closed on unknown migrations) still boots on
/// the newer schema. This image requires it: [`pending_migrations`] includes it.
pub struct GapClosureMigrator;
#[async_trait::async_trait]
impl MigratorTrait for GapClosureMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(self::m20261205_000033_client_session_binding::Migration),
            Box::new(self::m20261206_000034_forwarding_links_announcements_franking::Migration),
            Box::new(self::m20261207_000035_organization_moderation_team_channels::Migration),
            // ADR 0062 / 0063: partner session devices, the partner baseline, MESSAGE_SENT
            // facts and partner push (additive; every flag default-off).
            Box::new(self::m20261208_000036_partner_session_devices::Migration),
            // ADR 0064 CF Meet Apps: messages record the sending app (additive; the
            // MESSAGING_APPS_ENABLED flag is default-off).
            Box::new(self::m20261209_000037_app_messages::Migration),
        ]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(GAP_CLOSURE_MIGRATION_TABLE).into_iden()
    }
}

/// The gap-closure migrations not yet applied (read-only; fails closed on unknown ones).
pub async fn gap_closure_pending<C>(
    db: &C,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::Statement;
    let known: Vec<String> = GapClosureMigrator::migrations()
        .iter()
        .map(|m| m.name().to_string())
        .collect();
    let backend = db.get_database_backend();
    let present = db
        .query_one(Statement::from_string(
            backend,
            format!("SELECT to_regclass('{GAP_CLOSURE_MIGRATION_TABLE}') IS NOT NULL AS present"),
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
            format!("SELECT version FROM {GAP_CLOSURE_MIGRATION_TABLE}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::HashSet<_>, _>>()?;
    if applied.iter().any(|v| !known.contains(v)) {
        return Err(sea_orm_migration::sea_orm::DbErr::Custom(
            "a gap-closure migration is applied but unknown to this binary".into(),
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

mod notification_fence;
mod notification_publications;

/// Name of the table recording the routing fence history ([`NotificationFenceMigrator`]).
pub const NOTIFICATION_FENCE_MIGRATION_TABLE: &str =
    "messaging_notification_fence_seaql_migrations";

/// Routing fence protocol v1 (Notification Service specification, Document 4 §9): an
/// independently versioned, additive history (binary
/// `messaging-notification-fence-migration`), so a previous Messaging image — whose
/// [`Migrator`] fails closed on unknown migrations — still boots on a fenced database.
pub struct NotificationFenceMigrator;
#[async_trait::async_trait]
impl MigratorTrait for NotificationFenceMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(notification_fence::Migration),
            // N7b: Messaging's outbox of publications to the Notification Service.
            Box::new(notification_publications::Migration),
        ]
    }
    fn migration_table_name() -> DynIden {
        Alias::new(NOTIFICATION_FENCE_MIGRATION_TABLE).into_iden()
    }
}

/// Read-only: `true` when exactly the routing fence history this binary knows is applied.
/// Only `SELECT`s, so the least-privilege runtime role can call it.
pub async fn notification_fence_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT to_regclass('public.{NOTIFICATION_FENCE_MIGRATION_TABLE}') IS NOT NULL AS present"
            ),
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
            format!("SELECT version FROM {NOTIFICATION_FENCE_MIGRATION_TABLE}"),
        ))
        .await?;
    let known: std::collections::BTreeSet<_> = NotificationFenceMigrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(known == actual)
}

mod send_policy_projection;
pub struct SendPolicyProjectionMigrator;
#[async_trait::async_trait]
impl MigratorTrait for SendPolicyProjectionMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(send_policy_projection::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("messaging_send_policy_projection_migrations").into_iden()
    }
}

/// Read-only readiness for the independently versioned additive integration.
pub async fn send_policy_projection_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db.query_one(Statement::from_string(DbBackend::Postgres, "SELECT to_regclass('public.messaging_send_policy_projection_migrations') IS NOT NULL AS present".to_owned())).await?.ok_or_else(||sea_orm_migration::sea_orm::DbErr::Custom("readiness unavailable".into()))?.try_get::<bool>("","present")?;
    if !exists {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM messaging_send_policy_projection_migrations".to_owned(),
        ))
        .await?;
    let known: std::collections::BTreeSet<_> = SendPolicyProjectionMigrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(known == actual)
}
