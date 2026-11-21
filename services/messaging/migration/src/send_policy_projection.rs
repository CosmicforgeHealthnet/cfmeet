use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE messaging_send_block_projection (
 tenant_id UUID NOT NULL, application_id UUID NOT NULL, environment_id UUID NOT NULL,
 generation UUID NOT NULL,
 source TEXT NOT NULL CHECK (source IN ('COMMUNICATIONS','CALLING')),
 rule_id UUID NOT NULL, actor_principal_id UUID, target_principal_id UUID NOT NULL,
 source_version BIGINT NOT NULL CHECK (source_version > 0), active BOOLEAN NOT NULL,
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY (tenant_id,application_id,environment_id,generation,source,rule_id,target_principal_id),
 CHECK (actor_principal_id IS NULL OR actor_principal_id <> target_principal_id),
 CHECK (source = 'CALLING' OR actor_principal_id IS NOT NULL)
);
CREATE INDEX messaging_send_block_projection_pair ON messaging_send_block_projection
 (tenant_id,application_id,environment_id,generation,target_principal_id) WHERE active;
"#).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Send policy projection is additive and forward-only".into(),
        ))
    }
}
