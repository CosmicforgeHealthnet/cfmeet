//! Owner events to the HQ admin bridge (ADR 0068 A7; additive; appended to the
//! `messaging_admin_operations_seaql_migrations` history, which no released image reads).
//!
//! * `messaging_admin_bridge_events` — the transactional outbox of the events Messaging
//!   publishes to the bridge (`abuse.report.changed.v1`, `incident.changed.v1`): written
//!   in the SAME transaction as the report / incident change, only while
//!   `CF_ADMIN_BRIDGE_BASE_URL` is configured, and relayed at least once by a worker
//!   (the bridge dedupes on producer and `event_id`). Ids, states, versions and times only.
//! * `messaging_incidents.version` — the incident row version an event carries (an
//!   incident is raised at version 1; resolving it advances the version).
//!
//! Rollback: the previous image never reads the table; its incident inserts get the
//! column default and its resolve leaves the version unchanged.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_admin_bridge_events (
    event_id          UUID          NOT NULL,
    event_type        VARCHAR(64)   NOT NULL,
    subject_kind      VARCHAR(48)   NOT NULL,
    subject_id        UUID          NOT NULL,
    subject_state     VARCHAR(64)   NOT NULL,
    subject_version   BIGINT        NOT NULL,
    occurred_at       TIMESTAMPTZ   NOT NULL,
    delivery          VARCHAR(16)   NOT NULL DEFAULT 'PENDING',
    attempts          INTEGER       NOT NULL DEFAULT 0,
    next_attempt_at   TIMESTAMPTZ   NOT NULL,
    lease_token       UUID,
    lease_expires_at  TIMESTAMPTZ,
    created_at        TIMESTAMPTZ   NOT NULL,
    completed_at      TIMESTAMPTZ,
    CONSTRAINT pk_messaging_admin_bridge_events PRIMARY KEY (event_id),
    CONSTRAINT ck_messaging_admin_bridge_events_type CHECK (
        (event_type = 'abuse.report.changed.v1' AND subject_kind = 'abuse_report')
        OR (event_type = 'incident.changed.v1' AND subject_kind = 'incident')),
    CONSTRAINT ck_messaging_admin_bridge_events_state CHECK (subject_state ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_messaging_admin_bridge_events_version CHECK (subject_version >= 1),
    CONSTRAINT ck_messaging_admin_bridge_events_delivery CHECK (delivery IN ('PENDING', 'ACKED', 'DEAD')),
    CONSTRAINT ck_messaging_admin_bridge_events_settled CHECK ((delivery = 'PENDING') = (completed_at IS NULL)),
    CONSTRAINT ck_messaging_admin_bridge_events_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL))
);
CREATE INDEX ix_messaging_admin_bridge_events_due ON messaging_admin_bridge_events (next_attempt_at)
    WHERE delivery = 'PENDING';
CREATE INDEX ix_messaging_admin_bridge_events_settled ON messaging_admin_bridge_events (completed_at)
    WHERE delivery <> 'PENDING';

ALTER TABLE messaging_incidents
    ADD COLUMN version BIGINT NOT NULL DEFAULT 1,
    ADD CONSTRAINT ck_messaging_incidents_version CHECK (version >= 1);
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_incidents DROP CONSTRAINT IF EXISTS ck_messaging_incidents_version;
ALTER TABLE messaging_incidents DROP COLUMN IF EXISTS version;
DROP TABLE IF EXISTS messaging_admin_bridge_events;
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
