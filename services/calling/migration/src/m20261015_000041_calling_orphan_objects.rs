//! Orphaned protected objects (Part H §79.26).
//!
//! Voicemail / recording capture delivery writes the object BEFORE the state transaction.
//! When integrity validation or finalization then fails (or the row is no longer
//! awaiting it), the just-written object is deleted at once; when that deletion fails
//! too, the object is queued here and the recording / voicemail worker retries it with
//! backoff (`FOR UPDATE SKIP LOCKED`, bounded batches) until storage confirms the delete,
//! then removes the row (a bounded delete by id — the key is then stored nowhere).
//!
//! * `calling_orphan_objects` — the ONLY place an orphan's object key is kept: never in a
//!   log, metric, incident, audit row or event. One row per key (re-queueing is
//!   idempotent). `artifact_kind` / `artifact_id` name the owning row so a key that row
//!   still references (a later successful delivery) is never deleted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_orphan_objects (
    id              UUID         NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    artifact_kind   TEXT         NOT NULL,
    artifact_id     UUID         NOT NULL,
    object_key      VARCHAR(512) NOT NULL,
    reason          VARCHAR(64)  NOT NULL,
    attempts        INTEGER      NOT NULL,
    next_attempt_at TIMESTAMPTZ  NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    updated_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_calling_orphan_objects PRIMARY KEY (id),
    CONSTRAINT uq_calling_orphan_objects_key UNIQUE (object_key),
    CONSTRAINT fk_calling_orphan_objects_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_calling_orphan_objects_kind CHECK (artifact_kind IN ('RECORDING', 'VOICEMAIL')),
    CONSTRAINT ck_calling_orphan_objects_key CHECK (
        length(object_key) <= 512 AND object_key ~ '^[A-Za-z0-9][A-Za-z0-9/_.-]*$'),
    CONSTRAINT ck_calling_orphan_objects_reason CHECK (reason ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_calling_orphan_objects_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_calling_orphan_objects_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_calling_orphan_objects_due ON calling_orphan_objects (next_attempt_at);
CREATE INDEX idx_calling_orphan_objects_artifact ON calling_orphan_objects (artifact_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS calling_orphan_objects;
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
