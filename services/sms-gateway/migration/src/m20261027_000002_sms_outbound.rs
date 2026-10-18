//! Outbound SMS (Part IV Workstream O): the trusted environment kind of an outbound
//! message's sender, so the dispatcher can re-check — with a signed delegated context of
//! that exact scope — that Numbering still holds the sender as the scope's ACTIVE
//! assignment right before the provider is called. Set at authorization, never changed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE sms_transports
    ADD COLUMN environment_kind VARCHAR(16),
    ADD CONSTRAINT ck_sms_transports_environment_kind CHECK (
        (environment_kind IS NULL OR environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION'))
        AND (direction = 'INBOUND' OR environment_kind IS NOT NULL)) NOT VALID;

CREATE FUNCTION sms_guard_transport_kind() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.environment_kind IS DISTINCT FROM OLD.environment_kind THEN
        RAISE EXCEPTION 'sms transport facts are immutable' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_sms_transports_guard_kind
    BEFORE UPDATE ON sms_transports
    FOR EACH ROW EXECUTE FUNCTION sms_guard_transport_kind();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_sms_transports_guard_kind ON sms_transports;
DROP FUNCTION IF EXISTS sms_guard_transport_kind();
ALTER TABLE sms_transports
    DROP CONSTRAINT IF EXISTS ck_sms_transports_environment_kind,
    DROP COLUMN IF EXISTS environment_kind;
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
