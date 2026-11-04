use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection().execute_unprepared(r#"CREATE TABLE calling_communication_inbox (
 event_id UUID PRIMARY KEY,tenant_id UUID NOT NULL,application_id UUID NOT NULL,environment_id UUID NOT NULL,
 request_digest BYTEA NOT NULL CHECK(octet_length(request_digest)=32),received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE calling_communication_revocations (
 tenant_id UUID NOT NULL,application_id UUID NOT NULL,environment_id UUID NOT NULL,principal_id UUID NOT NULL,
 credentials_not_before BIGINT NOT NULL,updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant_id,application_id,environment_id,principal_id)
);
"#).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Communications integration is additive and forward-only".into(),
        ))
    }
}
