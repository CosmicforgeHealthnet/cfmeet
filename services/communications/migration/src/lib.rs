pub use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{DbBackend, Statement};
pub struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Foundation)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_seaql_migrations").into_iden()
    }
}
struct Foundation;
impl MigrationName for Foundation {
    fn name(&self) -> &str {
        "m20261103_000001_foundation"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Foundation {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/001_foundation.sql"))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Communications migrations are forward-only; retain data on application rollback"
                .into(),
        ))
    }
}
/// Sections 9–23 projections use a SEPARATE history so the previously deployed binary,
/// whose readiness requires exactly the foundation row, still starts after this
/// additive migration (rollback safety). Never merge the two histories.
pub struct UnifiedMigrator;
#[async_trait::async_trait]
impl MigratorTrait for UnifiedMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Unified)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_unified_seaql_migrations").into_iden()
    }
}
struct Unified;
impl MigrationName for Unified {
    fn name(&self) -> &str {
        "m20261106_000001_unified"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Unified {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/002_unified.sql"))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Communications migrations are forward-only; retain data on application rollback"
                .into(),
        ))
    }
}
/// Blueprint v1.0 Section 14 (ACC-008): device references bound to the Account session
/// that linked them. A THIRD separate history for the same reason as the unified one:
/// the previously deployed image checks exactly the foundation and unified histories and
/// must still start after this additive migration (rollback safety). Never merge them.
pub struct DeviceSessionMigrator;
#[async_trait::async_trait]
impl MigratorTrait for DeviceSessionMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(DeviceSessions)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_device_session_seaql_migrations").into_iden()
    }
}
struct DeviceSessions;
impl MigrationName for DeviceSessions {
    fn name(&self) -> &str {
        "m20261108_000001_device_session_binding"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for DeviceSessions {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/003_device_sessions.sql"))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Communications migrations are forward-only; retain data on application rollback"
                .into(),
        ))
    }
}
/// Audit closure MSG-009: pending call cards for call parties without a profile at
/// launch, and deduplicated, version-monotonic timeline nudges for Messaging. A FOURTH
/// separate history for the same rollback-safety reason as the others: the previous image
/// checks exactly the foundation, unified and device-session histories. Never merge them.
pub struct TimelineRealtimeMigrator;
#[async_trait::async_trait]
impl MigratorTrait for TimelineRealtimeMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(TimelineRealtime)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_timeline_realtime_seaql_migrations").into_iden()
    }
}
struct TimelineRealtime;
impl MigrationName for TimelineRealtime {
    fn name(&self) -> &str {
        "m20261109_000001_timeline_realtime"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for TimelineRealtime {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/004_timeline_realtime.sql"))
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Communications migrations are forward-only; retain data on application rollback"
                .into(),
        ))
    }
}
/// Apply the foundation history, then the unified, device-session and timeline-realtime
/// ones.
pub async fn up_all(db: &sea_orm_migration::sea_orm::DatabaseConnection) -> Result<(), DbErr> {
    Migrator::up(db, None).await?;
    UnifiedMigrator::up(db, None).await?;
    DeviceSessionMigrator::up(db, None).await?;
    TimelineRealtimeMigrator::up(db, None).await
}
/// Everything this image needs: the Sections 9-23 histories, the Section 14 one and the
/// MSG-009 timeline-realtime one.
pub async fn ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    if !device_session_ready(db).await? {
        return Ok(false);
    }
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_timeline_realtime_seaql_migrations') IS NOT NULL AS ready"
                .to_owned(),
        ))
        .await?;
    if !row
        .map(|r| r.try_get::<bool>("", "ready"))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM communications_timeline_realtime_seaql_migrations".to_owned(),
        ))
        .await?;
    Ok(rows.len() == 1 && rows[0].try_get::<String>("", "version")? == TimelineRealtime.name())
}
/// Readiness the previous (Section 14) image checks: foundation, unified and
/// device-session histories exactly.
pub async fn device_session_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    if !unified_ready(db).await? {
        return Ok(false);
    }
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_device_session_seaql_migrations') IS NOT NULL AS ready"
                .to_owned(),
        ))
        .await?;
    if !row
        .map(|r| r.try_get::<bool>("", "ready"))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM communications_device_session_seaql_migrations".to_owned(),
        ))
        .await?;
    Ok(rows.len() == 1 && rows[0].try_get::<String>("", "version")? == DeviceSessions.name())
}
/// Readiness the previous (Sections 9-23) image checks: foundation and unified exactly.
pub async fn unified_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    if !foundation_ready(db).await? {
        return Ok(false);
    }
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_unified_seaql_migrations') IS NOT NULL AS ready"
                .to_owned(),
        ))
        .await?;
    if !row
        .map(|r| r.try_get::<bool>("", "ready"))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM communications_unified_seaql_migrations".to_owned(),
        ))
        .await?;
    Ok(rows.len() == 1 && rows[0].try_get::<String>("", "version")? == Unified.name())
}
/// Readiness of the original foundation history alone (what the previous image checks).
pub async fn foundation_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    let row=db.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT to_regclass('communication_profiles') IS NOT NULL AND to_regclass('communications_seaql_migrations') IS NOT NULL AS ready".to_owned())).await?;
    if !row
        .map(|r| r.try_get::<bool>("", "ready"))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT version FROM communications_seaql_migrations".to_owned(),
        ))
        .await?;
    Ok(rows.len() == 1 && rows[0].try_get::<String>("", "version")? == Foundation.name())
}
