//! ADR 0068, phase A7 — owner events to the HQ admin bridge. Additive only, in the
//! admin-operations history (the previous image never reads it).
//!
//! `calling_admin_bridge_events` is Calling's transactional outbox of owner events for the
//! bridge's private event listener (`POST /internal/v1/hq-bridge/events`):
//! `abuse.report.changed.v1` (a calling abuse report was filed or reviewed) and
//! `incident.changed.v1` (a tenant's calling incident opened or was resolved). A row is
//! written in the SAME transaction as the state change, and only while
//! `CF_ADMIN_BRIDGE_BASE_URL` is configured (unset: nothing is stored or sent). The relay
//! posts `{event_id, type, subject {kind, id}, state, version, occurred_at}` until the
//! bridge answers; the bridge dedupes on (producer, event_id), so a re-post is harmless.
//!
//! Ids, codes, versions and times only: the subject is the report's / incident's opaque
//! id inside its tenant (`tenant_id` is kept for diagnostics and never sent). Never a
//! telephone number, SIP URI, principal, category, comment code, evidence or free text.
//! A platform-wide incident (no tenant) is never published. Settled rows are purged after
//! 30 days.
//!
//! Rollback: DOWN drops the table (unsent events are lost; the bridge resynchronises from
//! its own reads).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_admin_bridge_events (
    event_id         UUID          NOT NULL,
    event_type       VARCHAR(64)   NOT NULL,
    subject_kind     VARCHAR(32)   NOT NULL,
    subject_id       UUID          NOT NULL,
    tenant_id        UUID          NOT NULL,
    subject_state    VARCHAR(32)   NOT NULL,
    subject_version  BIGINT        NOT NULL,
    occurred_at      TIMESTAMPTZ   NOT NULL,
    delivery_state   VARCHAR(8)    NOT NULL DEFAULT 'PENDING',
    outcome          VARCHAR(16),
    last_error       VARCHAR(64),
    attempts         INTEGER       NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ   NOT NULL DEFAULT now(),
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT now(),
    completed_at     TIMESTAMPTZ,
    expires_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_calling_admin_bridge_events PRIMARY KEY (event_id),
    CONSTRAINT ck_calling_admin_bridge_events_type CHECK (
        (event_type = 'abuse.report.changed.v1' AND subject_kind = 'abuse_report'
            AND subject_state IN ('OPEN', 'REVIEWED', 'DISMISSED'))
        OR (event_type = 'incident.changed.v1' AND subject_kind = 'incident'
            AND subject_state IN ('OPEN', 'RESOLVED'))),
    CONSTRAINT ck_calling_admin_bridge_events_version CHECK (subject_version >= 1),
    CONSTRAINT ck_calling_admin_bridge_events_delivery
        CHECK (delivery_state IN ('PENDING', 'ACKED', 'DEAD', 'EXPIRED')),
    CONSTRAINT ck_calling_admin_bridge_events_outcome CHECK (outcome IS NULL
        OR outcome IN ('APPENDED', 'DUPLICATE', 'STALE', 'QUARANTINED', 'REJECTED')),
    CONSTRAINT ck_calling_admin_bridge_events_error
        CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_calling_admin_bridge_events_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_calling_admin_bridge_events_completed
        CHECK ((delivery_state = 'PENDING') = (completed_at IS NULL))
);
CREATE INDEX idx_calling_admin_bridge_events_due
    ON calling_admin_bridge_events (next_attempt_at, created_at) WHERE delivery_state = 'PENDING';
CREATE INDEX idx_calling_admin_bridge_events_settled
    ON calling_admin_bridge_events (completed_at) WHERE delivery_state <> 'PENDING';
CREATE INDEX idx_calling_admin_bridge_events_subject
    ON calling_admin_bridge_events (subject_kind, subject_id, subject_version);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS calling_admin_bridge_events;
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
