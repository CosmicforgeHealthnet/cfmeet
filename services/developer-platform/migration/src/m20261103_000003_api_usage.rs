//! Section 51 — durable API request accounting.
//!
//! The API Gateway enforces per-minute and per-day request quotas against shared atomic
//! Redis counters and periodically flushes the accepted counts here in idempotent batches:
//!
//! * `devplat_api_usage_daily` — accepted public API requests per application
//!   environment (Platform Access `environment_id`) per UTC day. The daily window of the
//!   gateway is re-seeded from this table after a Redis loss, so a day quota survives a
//!   counter-store restart.
//! * `devplat_api_usage_batches` — applied batch ids: a batch replayed after an unknown
//!   outcome is counted exactly once.
//!
//! Counts only: no request content, path parameter, credential or caller identity.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_api_usage_daily (
    environment_id  UUID        NOT NULL,
    usage_day       DATE        NOT NULL,
    request_count   BIGINT      NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_devplat_api_usage_daily PRIMARY KEY (environment_id, usage_day),
    CONSTRAINT ck_devplat_api_usage_daily_count CHECK (request_count >= 0)
);

CREATE TABLE devplat_api_usage_batches (
    batch_id        UUID        NOT NULL,
    recorded_by     VARCHAR(64) NOT NULL,
    entry_count     INTEGER     NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_devplat_api_usage_batches PRIMARY KEY (batch_id),
    CONSTRAINT ck_devplat_api_usage_batches_entries CHECK (entry_count BETWEEN 1 AND 10000)
);

-- Counters only grow; batches are immutable.
CREATE FUNCTION devplat_api_usage_daily_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'devplat_api_usage_daily: usage history is never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.environment_id <> OLD.environment_id OR NEW.usage_day <> OLD.usage_day OR NEW.request_count < OLD.request_count THEN
        RAISE EXCEPTION 'devplat_api_usage_daily: counters only grow' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_devplat_api_usage_daily_guard BEFORE UPDATE OR DELETE ON devplat_api_usage_daily
    FOR EACH ROW EXECUTE FUNCTION devplat_api_usage_daily_guard();
CREATE TRIGGER trg_devplat_api_usage_batches_append_only BEFORE UPDATE OR DELETE ON devplat_api_usage_batches
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_api_usage_batches;
DROP TABLE IF EXISTS devplat_api_usage_daily;
DROP FUNCTION IF EXISTS devplat_api_usage_daily_guard();
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
