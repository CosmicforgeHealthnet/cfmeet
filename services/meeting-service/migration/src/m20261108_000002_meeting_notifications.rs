//! Blueprint v1.0 Section 11 — meeting notifications (MTG-006, ACC-020).
//!
//! Part of the SEPARATE additive history `profile_notifications_migrations` (rollback safe:
//! the previous image never reads these tables). No existing table or column changes.
//!
//! * `meeting_notifications` — one durable job per notification (reschedule, cancellation,
//!   reminder). `dedupe_key` makes every producer idempotent: a reminder is keyed by
//!   conference + scheduled start, so several scheduler instances, restarts and repeated
//!   ticks create it once, and a reschedule (new start) gets a new reminder. Jobs are
//!   claimed with a lease (`FOR UPDATE SKIP LOCKED`) and settled with that lease.
//! * `meeting_notification_recipients` — per-recipient ledger keyed by the SHA-256 of the
//!   lowercased address (no address is stored), so a retried or re-claimed job never emails
//!   the same person twice.
//!
//! Payloads hold ids, times and a version only — never a token, passcode or address.
//! Recipients and preferences are resolved when the job is delivered. Finished jobs are
//! purged after 30 days by the notification worker.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE meeting_notifications (
    id               UUID PRIMARY KEY,
    kind             VARCHAR(32) NOT NULL CHECK (kind IN (
                         'CONFERENCE_RESCHEDULED', 'CONFERENCE_CANCELLED', 'CONFERENCE_REMINDER')),
    meeting_space_id UUID NOT NULL,
    conference_id    UUID NOT NULL,
    dedupe_key       VARCHAR(200) NOT NULL,
    actor_user_id    UUID NULL,
    payload          JSONB NOT NULL DEFAULT '{}'::jsonb
                         CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 2048),
    status           VARCHAR(16) NOT NULL DEFAULT 'PENDING'
                         CHECK (status IN ('PENDING', 'SENT', 'SKIPPED', 'FAILED')),
    attempts         INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_id         UUID NULL,
    lease_until      TIMESTAMPTZ NULL,
    last_error       VARCHAR(64) NULL,
    recipients_queued     INTEGER NOT NULL DEFAULT 0 CHECK (recipients_queued >= 0),
    recipients_suppressed INTEGER NOT NULL DEFAULT 0 CHECK (recipients_suppressed >= 0),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at     TIMESTAMPTZ NULL,
    CONSTRAINT uq_meeting_notifications_dedupe UNIQUE (dedupe_key),
    CONSTRAINT ck_meeting_notifications_completion CHECK ((status = 'PENDING') = (completed_at IS NULL))
);
CREATE INDEX idx_meeting_notifications_due
    ON meeting_notifications (available_at) WHERE status = 'PENDING';
CREATE INDEX idx_meeting_notifications_completed
    ON meeting_notifications (completed_at) WHERE completed_at IS NOT NULL;
CREATE INDEX idx_meeting_notifications_conference
    ON meeting_notifications (conference_id, created_at DESC);

CREATE TABLE meeting_notification_recipients (
    notification_id  UUID NOT NULL REFERENCES meeting_notifications (id) ON DELETE CASCADE,
    recipient_digest VARCHAR(64) NOT NULL CHECK (length(recipient_digest) = 64),
    outcome          VARCHAR(16) NOT NULL CHECK (outcome IN ('QUEUED', 'SUPPRESSED')),
    recorded_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (notification_id, recipient_digest)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS meeting_notification_recipients;
DROP TABLE IF EXISTS meeting_notifications;
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
