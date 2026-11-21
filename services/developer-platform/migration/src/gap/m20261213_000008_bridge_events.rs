//! ADR 0068, phase A7 — the outbox of owner events to the HQ admin bridge. Additive only.
//!
//! `devplat_bridge_events`: one row per owner event (`verification.case.changed.v1`,
//! `partner.workspace.changed.v1`, `app.review.changed.v1`, `pricing.version.changed.v1`,
//! `incident.changed.v1`), written in the transaction of the state change ONLY while
//! `CF_ADMIN_BRIDGE_BASE_URL` is set (unset: nothing is stored). Ids, states, versions and
//! times only, never personal data. One row per (type, subject, version). The worker posts
//! each event once to the bridge (`SENT`), dead-letters a body the bridge refuses
//! (`DEAD`), gives up after 7 days (`EXPIRED`) and purges settled rows after 7 days.
//!
//! Rollback: DOWN drops the table.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_bridge_events (
    id               UUID         NOT NULL,
    event_type       VARCHAR(48)  NOT NULL,
    subject_kind     VARCHAR(24)  NOT NULL,
    subject_id       VARCHAR(128) NOT NULL,
    state            VARCHAR(64)  NOT NULL,
    subject_version  BIGINT       NOT NULL,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    delivery_state   VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER      NOT NULL DEFAULT 0,
    last_error       VARCHAR(64),
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_devplat_bridge_events PRIMARY KEY (id),
    CONSTRAINT uq_devplat_bridge_events_subject_version UNIQUE (event_type, subject_id, subject_version),
    CONSTRAINT ck_devplat_bridge_events_type CHECK (event_type IN ('verification.case.changed.v1',
        'partner.workspace.changed.v1', 'app.review.changed.v1', 'pricing.version.changed.v1', 'incident.changed.v1')),
    CONSTRAINT ck_devplat_bridge_events_kind CHECK (subject_kind IN
        ('verification_case', 'partner_workspace', 'app', 'price_version', 'incident')),
    CONSTRAINT ck_devplat_bridge_events_subject CHECK (subject_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_devplat_bridge_events_state CHECK (state ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_devplat_bridge_events_version CHECK (subject_version >= 1),
    CONSTRAINT ck_devplat_bridge_events_delivery CHECK (delivery_state IN ('PENDING', 'SENT', 'DEAD', 'EXPIRED')),
    CONSTRAINT ck_devplat_bridge_events_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_bridge_events_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_devplat_bridge_events_done CHECK ((delivery_state = 'PENDING') = (completed_at IS NULL))
);
CREATE INDEX ix_devplat_bridge_events_due ON devplat_bridge_events (next_attempt_at)
    WHERE delivery_state = 'PENDING';
CREATE INDEX ix_devplat_bridge_events_settled ON devplat_bridge_events (completed_at)
    WHERE delivery_state <> 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_bridge_events;
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
