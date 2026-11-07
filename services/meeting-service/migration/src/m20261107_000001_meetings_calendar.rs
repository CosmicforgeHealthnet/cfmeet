//! Blueprint v1.0 Section 10 — Meeting Spaces, scheduling and the first-party calendar
//! (MTG-005, MTG-010, MTG-018, SEC-014).
//!
//! A SEPARATE additive history (`meetings_calendar_migrations`): the previously deployed
//! image checks the exact base, account-security and Communications histories, so it
//! keeps starting after this migration is applied (rollback safety). No existing table
//! or column changes:
//!
//! * `meeting_idempotency_keys` — `Idempotency-Key` replay records for v2 meeting-space
//!   and conference creation (key and request stored only as SHA-256 digests).
//! * Read indexes for the cross-space agenda (`GET /api/v2/me/conferences`) and the
//!   stale-occurrence expiry: the caller's participations, email-bound invitations and
//!   registrations, and a global `conference_sessions (scheduled_start_at)` index.
//!
//! No foreign keys point at base tables, so the base history's own `down` (CI migration
//! smoke) is never blocked; the indexes on base tables disappear with those tables.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE meeting_idempotency_keys (
    id              UUID PRIMARY KEY,
    actor_ref       VARCHAR(160) NOT NULL,
    operation       VARCHAR(96)  NOT NULL,
    key_digest      VARCHAR(64)  NOT NULL CHECK (length(key_digest) = 64),
    request_digest  VARCHAR(64)  NOT NULL CHECK (length(request_digest) = 64),
    resource_type   VARCHAR(32)  NOT NULL CHECK (resource_type IN ('MEETING_SPACE', 'CONFERENCE')),
    resource_id     UUID NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL,
    CONSTRAINT uq_meeting_idempotency UNIQUE (actor_ref, operation, key_digest),
    CONSTRAINT ck_meeting_idempotency_expiry CHECK (expires_at > created_at)
);
CREATE INDEX idx_meeting_idempotency_expiry ON meeting_idempotency_keys (expires_at);

CREATE INDEX IF NOT EXISTS idx_conference_scheduled_start
    ON conference_sessions (scheduled_start_at) WHERE scheduled_start_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_participants_user_agenda
    ON conference_participants (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_invitations_invitee_email
    ON meeting_invitations (lower(invitee_email)) WHERE invitee_email IS NOT NULL AND status = 'ACTIVE';
CREATE INDEX IF NOT EXISTS idx_registrations_user_agenda
    ON webinar_registrations (user_id) WHERE user_id IS NOT NULL AND status = 'REGISTERED';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_registrations_user_agenda;
DROP INDEX IF EXISTS idx_invitations_invitee_email;
DROP INDEX IF EXISTS idx_participants_user_agenda;
DROP INDEX IF EXISTS idx_conference_scheduled_start;
DROP TABLE IF EXISTS meeting_idempotency_keys;
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
