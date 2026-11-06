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
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('public.account_communications_facade_migrations') IS NOT NULL AS present"
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
            "SELECT version FROM account_communications_facade_migrations".to_owned(),
        ))
        .await?;
    let known: std::collections::BTreeSet<_> = CommunicationsFacadeMigrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(known == actual)
}
pub async fn communications_ready<C: sea_orm_migration::sea_orm::ConnectionTrait>(
    db: &C,
) -> Result<bool, sea_orm_migration::sea_orm::DbErr> {
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('public.account_communications_migrations') IS NOT NULL AS present"
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
            "SELECT version FROM account_communications_migrations".to_owned(),
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
    use sea_orm_migration::sea_orm::{DbBackend, Statement};
    let exists = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('public.account_security_migrations') IS NOT NULL AS present"
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
            "SELECT version FROM account_security_migrations".to_owned(),
        ))
        .await?;
    let known: std::collections::BTreeSet<_> = AccountSecurityMigrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(known == actual)
}
