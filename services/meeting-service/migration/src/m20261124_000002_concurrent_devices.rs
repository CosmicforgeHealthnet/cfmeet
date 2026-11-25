//! Remove the exclusive primary-mobile constraint without modifying any identity,
//! trust state, session, message or revocation. Explicit approval remains required.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS uq_account_devices_primary_mobile")
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("Concurrent-device migration is forward-only; restoring an exclusive constraint could reject or discard authorized installations".into()))
    }
}
