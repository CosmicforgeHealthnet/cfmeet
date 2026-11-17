//! Notification Service publications (roadmap N7, Document 4 §5.1 / §5.2 #1–10).
//!
//! `organization_notification_publications` is the relay state of the Organizations
//! producer of `cfmeet.notifications.event` envelopes. It is fed from the committed
//! `organization_outbox` record rows (destination `cfmeet.reconciliation`, written in the
//! transaction of every change) — never from a second write in the business transaction —
//! and only while `CF_NOTIFICATIONS_BASE_URL` is configured. One row per source event and
//! recipient chunk; the ids-only envelope is built once and stored, so a re-post is
//! byte-identical (the service answers `DUPLICATE`).
//!
//! * `event_id` (`evt_<32 hex>`) and the envelope's `route.decisionId` are derived from
//!   (`source_event_id`, `chunk`): deterministic per source event.
//! * `state`: `PENDING` → `ACKED` (ACCEPTED / DUPLICATE / HELD), `DEAD` (REJECTED,
//!   contract refusal, invalid envelope, attempts exhausted), `SKIPPED` (no recipient, no
//!   contract value), `EXPIRED` (not acknowledged within its lifetime). Settled rows are
//!   purged after 30 days.
//! * The envelope CHECK keeps the stored document ids-only: only the catalogue data keys
//!   of the ten Organizations types and never a sealed part.
//!
//! The partial index on `organization_outbox` serves the discovery of recent record rows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE organization_notification_publications (
    id                  UUID         NOT NULL,
    event_id            VARCHAR(64)  NOT NULL,
    source_event_id     UUID         NOT NULL,
    chunk               SMALLINT     NOT NULL DEFAULT 0,
    organization_id     UUID         NOT NULL,
    event_type          VARCHAR(64)  NOT NULL,
    envelope            JSONB,
    state               VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    outcome             VARCHAR(16),
    last_error          VARCHAR(64),
    attempts            INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ  NOT NULL,
    occurred_at         TIMESTAMPTZ  NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    expires_at          TIMESTAMPTZ  NOT NULL,
    completed_at        TIMESTAMPTZ,
    CONSTRAINT pk_organization_notification_publications PRIMARY KEY (id),
    CONSTRAINT uq_organization_notification_publications_event UNIQUE (event_id),
    CONSTRAINT uq_organization_notification_publications_source UNIQUE (source_event_id, chunk),
    CONSTRAINT ck_organization_notification_publications_event_id CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_organization_notification_publications_chunk CHECK (chunk >= 0 AND chunk < 1000),
    CONSTRAINT ck_organization_notification_publications_event_type CHECK (event_type IN (
        'organizations.invitation.created', 'organizations.invitation.accepted',
        'organizations.invitation.declined', 'organizations.invitation.revoked',
        'organizations.invitation.expired', 'organizations.membership.role_changed',
        'organizations.membership.suspended', 'organizations.membership.removed',
        'organizations.ownership.transferred', 'organizations.organization.suspended')),
    CONSTRAINT ck_organization_notification_publications_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_organization_notification_publications_outcome CHECK (outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    CONSTRAINT ck_organization_notification_publications_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_organization_notification_publications_settled CHECK ((state = 'PENDING') = (completed_at IS NULL)),
    CONSTRAINT ck_organization_notification_publications_envelope CHECK (
        (envelope IS NULL AND state IN ('SKIPPED', 'DEAD'))
        OR (envelope IS NOT NULL
            AND jsonb_typeof(envelope) = 'object'
            AND jsonb_typeof(envelope -> 'data') = 'object'
            AND NOT (envelope ? 'sealed')
            AND (envelope -> 'data') - ARRAY['invitationId', 'membershipId', 'organizationId', 'role'] = '{}'::jsonb))
);
CREATE INDEX ix_organization_notification_publications_due
    ON organization_notification_publications (next_attempt_at) WHERE state = 'PENDING';
CREATE INDEX ix_organization_notification_publications_settled
    ON organization_notification_publications (completed_at) WHERE state <> 'PENDING';
CREATE INDEX ix_organization_outbox_record_created
    ON organization_outbox (created_at) WHERE destination = 'cfmeet.reconciliation';
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS ix_organization_outbox_record_created;
DROP TABLE IF EXISTS organization_notification_publications;
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
