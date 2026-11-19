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
/// Notification Service roadmap N7: the Hub's outbox of publications to the Notification
/// Service (`communication_notification_publications`). A FIFTH separate history for the
/// same rollback-safety reason as the others: the previous image checks exactly the
/// foundation, unified, device-session and timeline-realtime histories. Never merge them.
pub struct NotificationPublicationsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for NotificationPublicationsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(NotificationPublications)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_notification_publications_seaql_migrations").into_iden()
    }
}
struct NotificationPublications;
impl MigrationName for NotificationPublications {
    fn name(&self) -> &str {
        "m20261117_000001_notification_publications"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for NotificationPublications {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/005_notification_publications.sql"))
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
/// Notification Service roadmap N10 (Phase 4.2): the explicit preference-authority switch
/// (`communication_preference_authority`, default `HUB`). A SIXTH separate history for the
/// same rollback-safety reason as the others: the previous image checks exactly the
/// foundation, unified, device-session, timeline-realtime and notification-publications histories.
pub struct PreferenceAuthorityMigrator;
#[async_trait::async_trait]
impl MigratorTrait for PreferenceAuthorityMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(PreferenceAuthority)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_preference_authority_seaql_migrations").into_iden()
    }
}
struct PreferenceAuthority;
impl MigrationName for PreferenceAuthority {
    fn name(&self) -> &str {
        "m20261117_000001_preference_authority"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for PreferenceAuthority {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/006_preference_authority.sql"))
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
/// Backend gap closure: S9 (separate group-invitation privacy setting) and S4 (operator
/// review of profile reports). A SEVENTH separate history for the same rollback-safety
/// reason as the others. Never merge them.
pub struct GroupInvitationPolicyMigrator;
#[async_trait::async_trait]
impl MigratorTrait for GroupInvitationPolicyMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(GroupInvitationPolicy),
            Box::new(ProfileReportReviews),
            Box::new(Reactivation),
        ]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_group_invitation_policy_seaql_migrations").into_iden()
    }
}
struct GroupInvitationPolicy;
impl MigrationName for GroupInvitationPolicy {
    fn name(&self) -> &str {
        "m20261118_000001_group_invitation_policy"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for GroupInvitationPolicy {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/006_group_invitation_policy.sql"))
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
struct ProfileReportReviews;
impl MigrationName for ProfileReportReviews {
    fn name(&self) -> &str {
        "m20261118_000002_profile_report_reviews"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for ProfileReportReviews {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/007_profile_report_reviews.sql"))
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
struct Reactivation;
impl MigrationName for Reactivation {
    fn name(&self) -> &str {
        "m20261118_000003_reactivation"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Reactivation {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/008_reactivation.sql"))
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
/// Whether the gap-closure history (S9, S4, A16) is fully applied.
pub async fn group_invitation_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_group_invitation_policy_seaql_migrations') IS NOT NULL AS ready"
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
            "SELECT version FROM communications_group_invitation_policy_seaql_migrations"
                .to_owned(),
        ))
        .await?;
    let applied: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(applied.len() == 3
        && applied.contains(&GroupInvitationPolicy.name().to_owned())
        && applied.contains(&ProfileReportReviews.name().to_owned())
        && applied.contains(&Reactivation.name().to_owned()))
}
/// Short CF Meet IDs (CF- plus 6 characters) and direct contact (contact_approval). An
/// EIGHTH separate history: the previous image checks the seventh one has exactly its three
/// migrations, so appending there would stop it after a rollback. Never merge them.
pub struct ShortIdsMigrator;
#[async_trait::async_trait]
impl MigratorTrait for ShortIdsMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(ShortIds)]
    }
    fn migration_table_name() -> DynIden {
        Alias::new("communications_short_ids_seaql_migrations").into_iden()
    }
}
struct ShortIds;
impl MigrationName for ShortIds {
    fn name(&self) -> &str {
        "m20261119_000001_short_ids_direct_contact"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for ShortIds {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(include_str!("../sql/009_short_ids_direct_contact.sql"))
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
/// Whether the short-ID history is fully applied.
pub async fn short_ids_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_short_ids_seaql_migrations') IS NOT NULL AS ready"
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
            "SELECT version FROM communications_short_ids_seaql_migrations".to_owned(),
        ))
        .await?;
    let applied: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "version"))
        .collect::<Result<_, _>>()?;
    Ok(applied.len() == 1 && applied.contains(&ShortIds.name().to_owned()))
}
/// Apply the foundation history, then the unified, device-session, timeline-realtime,
/// notification-publications, preference-authority and group-invitation ones.
pub async fn up_all(db: &sea_orm_migration::sea_orm::DatabaseConnection) -> Result<(), DbErr> {
    Migrator::up(db, None).await?;
    UnifiedMigrator::up(db, None).await?;
    DeviceSessionMigrator::up(db, None).await?;
    TimelineRealtimeMigrator::up(db, None).await?;
    NotificationPublicationsMigrator::up(db, None).await?;
    PreferenceAuthorityMigrator::up(db, None).await?;
    GroupInvitationPolicyMigrator::up(db, None).await?;
    ShortIdsMigrator::up(db, None).await
}
/// Everything this image needs: every earlier history and the N10 preference-authority
/// one.
pub async fn ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    if !notification_publications_ready(db).await?
        || !group_invitation_ready(db).await?
        || !short_ids_ready(db).await?
    {
        return Ok(false);
    }
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_preference_authority_seaql_migrations') IS NOT NULL AS ready"
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
            "SELECT version FROM communications_preference_authority_seaql_migrations".to_owned(),
        ))
        .await?;
    Ok(rows.len() == 1 && rows[0].try_get::<String>("", "version")? == PreferenceAuthority.name())
}
/// Readiness the previous (N7) image checks: every history up to the notification-publications
/// one exactly.
pub async fn notification_publications_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    if !timeline_ready(db).await? {
        return Ok(false);
    }
    let row = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT to_regclass('communications_notification_publications_seaql_migrations') IS NOT NULL AS ready"
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
            "SELECT version FROM communications_notification_publications_seaql_migrations"
                .to_owned(),
        ))
        .await?;
    Ok(rows.len() == 1
        && rows[0].try_get::<String>("", "version")? == NotificationPublications.name())
}
/// Readiness the MSG-009 image checks: foundation, unified, device-session and
/// timeline-realtime histories exactly.
pub async fn timeline_ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
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
