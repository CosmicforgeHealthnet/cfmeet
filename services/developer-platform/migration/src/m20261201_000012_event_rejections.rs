//! Part VI Chapter 17 (§17.1) — refused platform event publications.
//!
//! `devplat_event_rejections`: a producer's publication refused because it conflicts with
//! the event already recorded under the same `(producer, source event id)` (another type,
//! ownership scope, time or payload). Append-only; it holds the producer, the source event
//! id and a reason code only — never the envelope or its payload. Operators read it in the
//! ingestion health view.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_event_rejections (
    id               UUID          NOT NULL,
    producer         VARCHAR(64)   NOT NULL,
    source_event_id  VARCHAR(64)   NOT NULL,
    reason_code      VARCHAR(64)   NOT NULL,
    received_at      TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_event_rejections PRIMARY KEY (id),
    CONSTRAINT ck_devplat_event_rejections_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{2,63}$')
);
CREATE INDEX ix_devplat_event_rejections_recent ON devplat_event_rejections (received_at DESC);
CREATE TRIGGER tr_devplat_event_rejections_append_only BEFORE UPDATE OR DELETE ON devplat_event_rejections
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
"#;

const DOWN: &str = "DROP TABLE IF EXISTS devplat_event_rejections;";

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
