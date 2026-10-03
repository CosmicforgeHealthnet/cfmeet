//! Phase 2 A–D closure — recurrence exceptions independent of materialization.
//!
//! An exception targets one occurrence of a recurrence rule by its ORIGINAL wall-clock
//! start in the series timezone (DST- and timezone-change-stable). The materializer
//! applies exceptions when it generates the ConferenceSession, so an occurrence years
//! ahead can be cancelled/rescheduled without generating rows beyond the horizon.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE meeting_recurrence_exceptions (
    id                     UUID PRIMARY KEY,
    recurrence_rule_id     UUID NOT NULL REFERENCES meeting_recurrence_rules(id) ON DELETE RESTRICT,
    meeting_space_id       UUID NOT NULL REFERENCES meeting_spaces(id) ON DELETE RESTRICT,
    original_start_local   TIMESTAMP NOT NULL,
    kind                   TEXT NOT NULL CHECK (kind IN ('CANCELLED','RESCHEDULED')),
    new_start_at           TIMESTAMPTZ,
    new_end_at             TIMESTAMPTZ,
    created_by             VARCHAR(128),
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    version                INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    CONSTRAINT uq_recurrence_exception UNIQUE (recurrence_rule_id, original_start_local),
    CHECK ((kind = 'RESCHEDULED') = (new_start_at IS NOT NULL)),
    CHECK (new_end_at IS NULL OR (new_start_at IS NOT NULL AND new_end_at > new_start_at))
);
CREATE INDEX idx_recurrence_exceptions_space ON meeting_recurrence_exceptions (meeting_space_id);
"#;

const DOWN: &str = "DROP TABLE IF EXISTS meeting_recurrence_exceptions;";

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
