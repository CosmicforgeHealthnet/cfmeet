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
pub async fn ready<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
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
