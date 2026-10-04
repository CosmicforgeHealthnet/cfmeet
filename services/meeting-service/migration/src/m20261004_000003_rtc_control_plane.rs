//! Phase 2 Part E — RTC control plane & realtime state (additive).
//!
//! - `outbox_events.channel` separates client realtime events from internal provider
//!   commands (clients never receive `PROVIDER_COMMAND` rows).
//! - `outbox_events.conference_seq` is a gap-free, commit-ordered per-conference
//!   sequence (allocated under the conference row lock) used as the realtime replay
//!   cursor; `locked_by` records the relay lease holder.
//! - `conference_sessions.event_seq` is the per-conference sequence counter;
//!   `rtc_room_ready_at` / `rtc_room_terminated_at` record provider confirmations
//!   (PREPARING → WAITING and ENDING → ENDED require them).
//! - `rtc_webhook_events` persists provider webhook ids for deduplication.
//! - `rtc_drift_events` records reconciler findings (repaired / escalated).
//! - `worker_leases` gives singleton workers (reconciler) a crash-safe lease.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE outbox_events
    ADD COLUMN channel TEXT NOT NULL DEFAULT 'REALTIME'
        CHECK (channel IN ('REALTIME','PROVIDER_COMMAND')),
    ADD COLUMN conference_seq BIGINT CHECK (conference_seq IS NULL OR conference_seq > 0),
    ADD COLUMN locked_by VARCHAR(128);
UPDATE outbox_events SET channel = 'PROVIDER_COMMAND' WHERE event_type LIKE 'rtc.%';
CREATE UNIQUE INDEX uq_outbox_conference_seq
    ON outbox_events (conference_id, conference_seq) WHERE conference_seq IS NOT NULL;
CREATE INDEX idx_outbox_pending_channel
    ON outbox_events (channel, available_at) WHERE status = 'PENDING';
CREATE INDEX idx_outbox_conference_replay
    ON outbox_events (conference_id, conference_seq) WHERE channel = 'REALTIME';

ALTER TABLE conference_sessions
    ADD COLUMN event_seq BIGINT NOT NULL DEFAULT 0 CHECK (event_seq >= 0),
    ADD COLUMN rtc_room_ready_at TIMESTAMPTZ,
    ADD COLUMN rtc_room_terminated_at TIMESTAMPTZ;

CREATE TABLE rtc_webhook_events (
    event_id        VARCHAR(160) PRIMARY KEY,
    provider        VARCHAR(32) NOT NULL,
    event_type      VARCHAR(64) NOT NULL,
    room_name       VARCHAR(128),
    conference_id   UUID,
    outcome         VARCHAR(48) NOT NULL,
    received_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_rtc_webhook_events_conference ON rtc_webhook_events (conference_id, received_at);

CREATE TABLE rtc_drift_events (
    id              UUID PRIMARY KEY,
    conference_id   UUID,
    room_name       VARCHAR(128),
    kind            VARCHAR(64) NOT NULL,
    action          VARCHAR(16) NOT NULL CHECK (action IN ('REPAIRED','ESCALATED','FAILED')),
    detail          JSONB NOT NULL DEFAULT '{}'::jsonb,
    detected_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_rtc_drift_events_conference ON rtc_drift_events (conference_id, detected_at);

CREATE TABLE worker_leases (
    name            VARCHAR(64) PRIMARY KEY,
    holder          VARCHAR(128) NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS worker_leases;
DROP TABLE IF EXISTS rtc_drift_events;
DROP TABLE IF EXISTS rtc_webhook_events;
ALTER TABLE conference_sessions
    DROP COLUMN IF EXISTS rtc_room_terminated_at,
    DROP COLUMN IF EXISTS rtc_room_ready_at,
    DROP COLUMN IF EXISTS event_seq;
DROP INDEX IF EXISTS idx_outbox_conference_replay;
DROP INDEX IF EXISTS idx_outbox_pending_channel;
DROP INDEX IF EXISTS uq_outbox_conference_seq;
ALTER TABLE outbox_events
    DROP COLUMN IF EXISTS locked_by,
    DROP COLUMN IF EXISTS conference_seq,
    DROP COLUMN IF EXISTS channel;
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
