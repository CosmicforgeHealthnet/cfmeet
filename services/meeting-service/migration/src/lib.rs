pub use sea_orm_migration::prelude::*;

// Import all migration modules
mod m20260116_000001_create_users;
mod m20260116_000002_create_meetings;
mod m20260116_000003_create_participants;
mod m20260116_000004_create_audio_video_devices;
mod m20260116_000005_create_chat_messages;
mod m20260116_000006_create_session_logs;
mod m20260116_000007_create_webhooks;
mod m20260116_000008_create_api_keys;
mod m20260117_000001_create_email_jobs;
mod m20260117_000002_add_reset_token_to_users;
mod m20260127_000001_create_oauth_states;
mod m20260205_110126_m20260205_000001_add_guest_role;
mod m20260206_000001_add_media_toggle_event_type;
mod m20260323_000001_migrate_to_redis;
mod m20261003_000001_phase2_meeting_domain;
mod m20261003_000002_phase2_legacy_backfill;
mod m20261004_000001_organizations;
mod m20261004_000002_recurrence_exceptions;
mod m20261004_000003_rtc_control_plane;
mod m20261004_000004_collaboration;
mod m20261005_000005_artifacts_profiles_operations;
mod m20261005_000006_message_main_cursor_index;
mod m20261019_000001_unified_messaging_integration;
mod m20261020_000001_unified_messaging_scoped_chat;
mod m20261101_000001_platform_environment_meetings;
mod m20261103_000001_communications_account_authority;
mod m20261105_000001_communications_meeting_facade;
mod m20261106_000001_account_security;
mod m20261107_000001_meetings_calendar;
mod m20261108_000001_account_profile;
mod m20261108_000002_meeting_notifications;
mod m20261109_000001_account_hardening;
mod m20261110_000001_organization_integration;

pub mod phase2_backfill;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260116_000001_create_users::Migration),
            Box::new(m20260116_000002_create_meetings::Migration),
            Box::new(m20260116_000003_create_participants::Migration),
            Box::new(m20260116_000004_create_audio_video_devices::Migration),
            Box::new(m20260116_000005_create_chat_messages::Migration),
            Box::new(m20260116_000006_create_session_logs::Migration),
            Box::new(m20260116_000007_create_webhooks::Migration),
            Box::new(m20260116_000008_create_api_keys::Migration),
            Box::new(m20260117_000001_create_email_jobs::Migration),
            Box::new(m20260117_000002_add_reset_token_to_users::Migration),
            Box::new(m20260127_000001_create_oauth_states::Migration),
            Box::new(m20260205_110126_m20260205_000001_add_guest_role::Migration),
            Box::new(m20260206_000001_add_media_toggle_event_type::Migration),
            Box::new(m20260323_000001_migrate_to_redis::Migration),
            Box::new(m20261003_000001_phase2_meeting_domain::Migration),
            Box::new(m20261003_000002_phase2_legacy_backfill::Migration),
            Box::new(m20261004_000001_organizations::Migration),
            Box::new(m20261004_000002_recurrence_exceptions::Migration),
            Box::new(m20261004_000003_rtc_control_plane::Migration),
            Box::new(m20261004_000004_collaboration::Migration),
            Box::new(m20261005_000005_artifacts_profiles_operations::Migration),
            Box::new(m20261005_000006_message_main_cursor_index::Migration),
            Box::new(m20261019_000001_unified_messaging_integration::Migration),
            Box::new(m20261020_000001_unified_messaging_scoped_chat::Migration),
            Box::new(m20261101_000001_platform_environment_meetings::Migration),
        ]
    }
}

/// The migrations of history `M` not yet applied — READ-ONLY (DATA-011 target rule,
/// `docs/architecture/migration-conventions.md`).
///
/// Readiness is "applied ⊇ migrations known to this binary, and no unknown versions":
/// the result lists what this binary needs but the database lacks, and a version applied
/// to the database but unknown to this binary is an error (fail closed), the same rule as
/// the `pending_migrations` helpers of calling, messaging and developer-platform. A
/// missing history table means everything is pending. It only `SELECT`s, so the
/// least-privilege runtime role can call it (`get_pending_migrations` would issue DDL).
pub async fn pending_migrations<M, C>(
    db: &C,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    M: MigratorTrait,
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    let known: Vec<String> = M::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    pending_in(db, &M::migration_table_name().to_string(), known).await
}

/// [`pending_migrations`] over an explicit history table and known list.
pub async fn pending_in<C>(
    db: &C,
    table: &str,
    known: Vec<String>,
) -> Result<Vec<String>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    use sea_orm_migration::sea_orm::{DbBackend, DbErr, Statement, Value};
    if !table
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(DbErr::Custom(format!(
            "invalid migration history table {table:?}"
        )));
    }
    let present = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT to_regclass('public.' || $1) IS NOT NULL AS present",
            [Value::from(table.to_owned())],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("readiness unavailable".into()))?
        .try_get::<bool>("", "present")?;
    if !present {
        return Ok(known);
    }
    let applied = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT version FROM {table}"),
        ))
        .await?
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let unknown: Vec<&String> = applied.iter().filter(|v| !known.contains(v)).collect();
    if !unknown.is_empty() {
        return Err(DbErr::Custom(format!(
            "migration history {table} has versions unknown to this binary: {}",
            unknown
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(known.into_iter().filter(|m| !applied.contains(m)).collect())
}

/// Every migration history of this binary, base first, as `(history table, pending)`.
/// Errors when any history carries a version unknown to this binary.
pub async fn migration_readiness<C>(
    db: &C,
) -> Result<Vec<(String, Vec<String>)>, sea_orm_migration::sea_orm::DbErr>
where
    C: sea_orm_migration::sea_orm::ConnectionTrait,
{
    Ok(vec![
        (
            Migrator::migration_table_name().to_string(),
            pending_migrations::<Migrator, _>(db).await?,
        ),
        (
            CommunicationsMigrator::migration_table_name().to_string(),
            pending_migrations::<CommunicationsMigrator, _>(db).await?,
        ),
        (
            CommunicationsFacadeMigrator::migration_table_name().to_string(),
            pending_migrations::<CommunicationsFacadeMigrator, _>(db).await?,
        ),
        (
            AccountSecurityMigrator::migration_table_name().to_string(),
            pending_migrations::<AccountSecurityMigrator, _>(db).await?,
        ),
        (
            MeetingsCalendarMigrator::migration_table_name().to_string(),
            pending_migrations::<MeetingsCalendarMigrator, _>(db).await?,
        ),
        (
            ProfileNotificationsMigrator::migration_table_name().to_string(),
            pending_migrations::<ProfileNotificationsMigrator, _>(db).await?,
        ),
        (
            AccountHardeningMigrator::migration_table_name().to_string(),
            pending_migrations::<AccountHardeningMigrator, _>(db).await?,
        ),
    ])
}

/// The base history (`seaql_migrations`) satisfies the target rule.
pub async fn base_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    Ok(pending_migrations::<Migrator, _>(db).await?.is_empty())
}

/// Additive personal-communications integration history. The base migration history
/// stays compatible with the retained pre-Hub application image on rollback.
pub struct CommunicationsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for CommunicationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            m20261103_000001_communications_account_authority::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("account_communications_migrations").into_iden()
    }
}

/// Read-only readiness for the independently versioned additive integration.
/// Sections 9-23 Meetings facade (Hub-initiated instant meetings). A SEPARATE history:
/// the previously deployed image requires the exact Communications set above, so it
/// still starts after this additive migration is applied (rollback safety).
pub struct CommunicationsFacadeMigrator;
#[async_trait::async_trait]
impl MigratorTrait for CommunicationsFacadeMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(
            m20261105_000001_communications_meeting_facade::Migration,
        )]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("account_communications_facade_migrations").into_iden()
    }
}
/// Exactly the known facade history has been applied.
pub async fn communications_facade_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    // DATA-011 target rule (applied ⊇ known, unknown versions fail closed); for a
    // separate history this equals the former exact-set check.
    Ok(pending_migrations::<CommunicationsFacadeMigrator, _>(db)
        .await?
        .is_empty())
}
pub async fn communications_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    // DATA-011 target rule (applied ⊇ known, unknown versions fail closed); for a
    // separate history this equals the former exact-set check.
    Ok(pending_migrations::<CommunicationsMigrator, _>(db)
        .await?
        .is_empty())
}

/// Blueprint v1.0 Section 2 account security (revocation epoch independent of the
/// Communications integration, sessions, security audit). A SEPARATE additive history so
/// the previously deployed image still starts after it is applied (rollback safety).
pub struct AccountSecurityMigrator;
#[async_trait::async_trait]
impl MigratorTrait for AccountSecurityMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20261106_000001_account_security::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("account_security_migrations").into_iden()
    }
}

/// Exactly the known account-security history has been applied.
pub async fn account_security_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    // DATA-011 target rule (applied ⊇ known, unknown versions fail closed); for a
    // separate history this equals the former exact-set check.
    Ok(pending_migrations::<AccountSecurityMigrator, _>(db)
        .await?
        .is_empty())
}

/// Blueprint v1.0 Section 10 meetings calendar (idempotency records, agenda indexes). A
/// SEPARATE additive history so the previously deployed image still starts after it is
/// applied (rollback safety).
pub struct MeetingsCalendarMigrator;
#[async_trait::async_trait]
impl MigratorTrait for MeetingsCalendarMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20261107_000001_meetings_calendar::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("meetings_calendar_migrations").into_iden()
    }
}

/// Exactly the known meetings-calendar history has been applied.
pub async fn meetings_calendar_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    // DATA-011 target rule (applied ⊇ known, unknown versions fail closed); for a
    // separate history this equals the former exact-set check.
    Ok(pending_migrations::<MeetingsCalendarMigrator, _>(db)
        .await?
        .is_empty())
}

/// Blueprint v1.0 Sections 9 and 11: the personal account profile (nullable `users`
/// columns) and durable meeting notification jobs. A SEPARATE additive history so the
/// previously deployed image still starts after it is applied (rollback safety).
pub struct ProfileNotificationsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for ProfileNotificationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261108_000001_account_profile::Migration),
            Box::new(m20261108_000002_meeting_notifications::Migration),
        ]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("profile_notifications_migrations").into_iden()
    }
}

/// Exactly the known profile/notifications history has been applied.
pub async fn profile_notifications_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    // DATA-011 target rule (applied ⊇ known, unknown versions fail closed); for a
    // separate history this equals the former exact-set check.
    Ok(pending_migrations::<ProfileNotificationsMigrator, _>(db)
        .await?
        .is_empty())
}

/// Blueprint v1.0 Sections 2–3 hardening: hashed legacy API keys and webhook secrets
/// (PLT-002, DATA-003). A SEPARATE additive history so the previously deployed image still
/// starts after it is applied (rollback safety).
pub struct AccountHardeningMigrator;
#[async_trait::async_trait]
impl MigratorTrait for AccountHardeningMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20261109_000001_account_hardening::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("account_hardening_migrations").into_iden()
    }
}

/// The account-hardening history satisfies the DATA-011 target rule (applied ⊇ known,
/// unknown versions fail closed).
pub async fn account_hardening_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    Ok(pending_migrations::<AccountHardeningMigrator, _>(db)
        .await?
        .is_empty())
}

/// Final closure: Organizations event inbox, aggregate fences and organization member
/// access (offboarding). A SEPARATE additive history so the previously deployed image
/// still starts after it is applied (rollback safety).
pub struct OrganizationIntegrationMigrator;
#[async_trait::async_trait]
impl MigratorTrait for OrganizationIntegrationMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20261110_000001_organization_integration::Migration)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("organization_integration_migrations").into_iden()
    }
}

/// The organization-integration history satisfies the DATA-011 target rule.
pub async fn organization_integration_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    Ok(pending_migrations::<OrganizationIntegrationMigrator, _>(db)
        .await?
        .is_empty())
}
