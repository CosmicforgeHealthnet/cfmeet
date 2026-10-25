//! Section 51 — normalized API_OPERATION usage (Appendix D: "1 operation with
//! service / operation / status dimensions", produced by the API Gateway).
//!
//! `devplat_api_operation_usage` counts accepted public API operations per application
//! environment, UTC day, owning domain, contract operation id and response status class.
//! The gateway flushes them in the same idempotent batches as the daily request counters
//! (`devplat_api_usage_batches`), so a replayed batch is counted exactly once. Counts and
//! catalogue identifiers only: no path parameter, body, credential or caller identity.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_api_operation_usage (
    environment_id  UUID         NOT NULL,
    usage_day       DATE         NOT NULL,
    domain          VARCHAR(40)  NOT NULL,
    operation_id    VARCHAR(120) NOT NULL,
    status_class    VARCHAR(3)   NOT NULL,
    request_count   BIGINT       NOT NULL,
    updated_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_api_operation_usage PRIMARY KEY (environment_id, usage_day, domain, operation_id, status_class),
    CONSTRAINT ck_devplat_api_operation_usage_count CHECK (request_count >= 0),
    CONSTRAINT ck_devplat_api_operation_usage_class CHECK (status_class IN ('2xx', '3xx', '4xx', '5xx')),
    CONSTRAINT ck_devplat_api_operation_usage_domain CHECK (domain ~ '^[a-z][a-z-]{0,39}$'),
    CONSTRAINT ck_devplat_api_operation_usage_operation CHECK (operation_id ~ '^[a-z][A-Za-z0-9_.-]{0,119}$')
);

CREATE FUNCTION devplat_api_operation_usage_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'devplat_api_operation_usage: usage history is never deleted' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.environment_id <> OLD.environment_id OR NEW.usage_day <> OLD.usage_day OR NEW.domain <> OLD.domain
       OR NEW.operation_id <> OLD.operation_id OR NEW.status_class <> OLD.status_class
       OR NEW.request_count < OLD.request_count THEN
        RAISE EXCEPTION 'devplat_api_operation_usage: counters only grow' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_devplat_api_operation_usage_guard BEFORE UPDATE OR DELETE ON devplat_api_operation_usage
    FOR EACH ROW EXECUTE FUNCTION devplat_api_operation_usage_guard();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_api_operation_usage;
DROP FUNCTION IF EXISTS devplat_api_operation_usage_guard();
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
