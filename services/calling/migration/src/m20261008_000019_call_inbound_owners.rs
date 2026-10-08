//! Phone-to-app ownership (Part E §45, §89).
//!
//! A call that arrives from the public network (Telephony offers it for a relay number)
//! is not created by a customer credential. The environment's operator names, once, the
//! service account / API credential the inbound Calls are recorded against: attribution,
//! concurrent-call limits and spend count there. No row (or a disabled row) = inbound
//! public calls are refused for that environment (fail closed).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE call_inbound_owners (
    tenant_id          UUID        NOT NULL,
    application_id     UUID        NOT NULL,
    environment_id     UUID        NOT NULL,
    service_account_id UUID        NOT NULL,
    credential_id      UUID        NOT NULL,
    enabled            BOOLEAN     NOT NULL,
    revision           INTEGER     NOT NULL,
    created_at         TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_inbound_owners PRIMARY KEY (environment_id),
    CONSTRAINT fk_call_inbound_owners_credential
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id, credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_call_inbound_owners_revision CHECK (revision >= 1),
    CONSTRAINT ck_call_inbound_owners_timestamps CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_call_inbound_owners_immutable
    BEFORE UPDATE ON call_inbound_owners
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'tenant_id', 'application_id', 'environment_id', 'created_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_inbound_owners;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
