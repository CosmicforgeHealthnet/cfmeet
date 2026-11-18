//! Roadmap N10 (Document 5 §6, Phase 4.2): the preference copy + catch-up job.
//!
//! * `notif_preference_copy` — ONE row: the operator switch (`mode` OFF / RUN, changed by
//!   the release tooling, version-guarded) and the job's durable progress: the snapshot
//!   phase / kind / cursor (resumable after a restart or on the other replica), the Hub
//!   change-feed cursor of the catch-up, counters, and the last verification report
//!   (counts per kind; requested with `verify_requested_at`).
//! * `notif_preference_change_log.origin` — who made a service-side write: `RELAY` (the
//!   Meeting relay, U7 directly) or `HUB_FACADE` (the Hub preference facade, which keeps
//!   the Hub mirror current). A rollback of the facade switch needs a write-back only for
//!   `RELAY` entries made after the switch.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE notif_preference_copy (
    singleton             BOOLEAN     NOT NULL DEFAULT true,
    mode                  VARCHAR(8)  NOT NULL DEFAULT 'OFF',
    phase                 VARCHAR(16) NOT NULL DEFAULT 'SNAPSHOT',
    snapshot_kind         VARCHAR(16) NOT NULL DEFAULT 'ACCOUNT',
    snapshot_cursor       VARCHAR(80),
    feed_cursor           BIGINT,
    snapshot_started_at   TIMESTAMPTZ,
    snapshot_completed_at TIMESTAMPTZ,
    snapshots_completed   INTEGER     NOT NULL DEFAULT 0,
    last_catch_up_at      TIMESTAMPTZ,
    rows_written          BIGINT      NOT NULL DEFAULT 0,
    rows_unchanged        BIGINT      NOT NULL DEFAULT 0,
    rows_skipped          BIGINT      NOT NULL DEFAULT 0,
    changes_applied       BIGINT      NOT NULL DEFAULT 0,
    verify_requested_at   TIMESTAMPTZ,
    verified_at           TIMESTAMPTZ,
    verification          JSONB,
    last_error            VARCHAR(64),
    last_error_at         TIMESTAMPTZ,
    version               BIGINT      NOT NULL DEFAULT 1,
    changed_by_run        VARCHAR(64) NOT NULL DEFAULT 'migration',
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_notif_preference_copy PRIMARY KEY (singleton),
    CONSTRAINT ck_notif_preference_copy_singleton CHECK (singleton),
    CONSTRAINT ck_notif_preference_copy_mode CHECK (mode IN ('OFF', 'RUN')),
    CONSTRAINT ck_notif_preference_copy_phase CHECK (phase IN ('SNAPSHOT', 'CATCH_UP')),
    CONSTRAINT ck_notif_preference_copy_kind CHECK (snapshot_kind IN ('ACCOUNT', 'CONVERSATION', 'CONTACT')),
    CONSTRAINT ck_notif_preference_copy_verification CHECK (verification IS NULL OR pg_column_size(verification) <= 4096)
);
INSERT INTO notif_preference_copy (singleton) VALUES (true);

ALTER TABLE notif_preference_change_log
    ADD COLUMN origin VARCHAR(16) NOT NULL DEFAULT 'RELAY',
    ADD CONSTRAINT ck_notif_preference_change_log_origin CHECK (origin IN ('RELAY', 'HUB_FACADE'));
"#;

const DOWN: &str = r#"
ALTER TABLE notif_preference_change_log DROP CONSTRAINT ck_notif_preference_change_log_origin;
ALTER TABLE notif_preference_change_log DROP COLUMN origin;
DROP TABLE notif_preference_copy;
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
