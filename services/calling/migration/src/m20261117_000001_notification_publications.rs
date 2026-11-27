//! Notification Service roadmap N7 (Document 4 §5.1, §9.5; D-10): Calling's producer
//! outbox of publications to the Notification Service's I1. Additive and forward-only, in
//! its OWN history (`calling_notification_publications_seaql_migrations`, see
//! [`crate::NotificationPublicationsMigrator`]): the previous image checks exactly the
//! main and Communications histories, so it stays ready on a database that has this one.
//!
//! One row per (fact, recipient), written in the SAME transaction as the state change, and
//! only while `CF_NOTIFICATIONS_BASE_URL` is configured:
//! * `calling.call.participant-missed` — a RECIPIENT participant whose offers all ended
//!   unanswered when the Call reached its terminal state (the ADR 0025 history
//!   derivation; `source_id` = the participant);
//! * `calling.voicemail.ready` — a voicemail message became READY, to each principal who
//!   owns the mailbox (`source_id` = the message);
//! * `calling.recording.ready` — a call recording became READY, to each principal who
//!   took part in the Call and may retrieve it (`source_id` = the recording).
//!
//! The relay posts the stored `cfmeet.notifications.event` envelope (built once, ids only:
//! the call id, the call kind, the message / recording id, the recipient's CF Meet account
//! reference; never a name, number, label, object key or media) byte-identically until I1
//! answers. ACCEPTED / DUPLICATE / HELD acknowledge the row (ACKED); REJECTED or a contract
//! refusal of the batch dead-letters it (DEAD); a recipient without a CF Meet account, an
//! inactive recipient or an artifact no longer READY is SKIPPED with its reason; a row
//! past its expiry is EXPIRED. Settled rows are purged after 30 days.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_notification_publications (
    id                     UUID         NOT NULL,
    event_id               VARCHAR(36)  NOT NULL,
    event_type             VARCHAR(64)  NOT NULL,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    call_id                UUID         NOT NULL,
    -- The fact: the missed participant, the voicemail message or the recording.
    source_id              UUID         NOT NULL,
    recipient_principal_id UUID         NOT NULL,
    envelope               JSONB,
    state                  VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    outcome                VARCHAR(9),
    last_error             VARCHAR(64),
    attempts               INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    occurred_at            TIMESTAMPTZ  NOT NULL DEFAULT now(),
    created_at             TIMESTAMPTZ  NOT NULL DEFAULT now(),
    completed_at           TIMESTAMPTZ,
    expires_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_calling_notification_publications PRIMARY KEY (id),
    CONSTRAINT uq_calling_notification_publications_event UNIQUE (event_id),
    -- One publication per fact, type and recipient: replays never write a second row.
    CONSTRAINT uq_calling_notification_publications_fact
        UNIQUE (source_id, event_type, recipient_principal_id),
    -- Tenant / environment isolation: the Call and the recipient belong to the row's scope.
    CONSTRAINT fk_calling_notification_publications_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_notification_publications_recipient
        FOREIGN KEY (tenant_id, application_id, environment_id, recipient_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_calling_notification_publications_event_id
        CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_calling_notification_publications_type CHECK (event_type IN (
        'calling.call.participant-missed', 'calling.voicemail.ready', 'calling.recording.ready')),
    CONSTRAINT ck_calling_notification_publications_state
        CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_calling_notification_publications_outcome
        CHECK (outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    CONSTRAINT ck_calling_notification_publications_error
        CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_calling_notification_publications_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_calling_notification_publications_completed
        CHECK ((state = 'PENDING') = (completed_at IS NULL)),
    -- A row waiting for the relay always holds the envelope it will post.
    CONSTRAINT ck_calling_notification_publications_pending
        CHECK (state <> 'PENDING' OR envelope IS NOT NULL),
    -- Ids only: exactly the catalogue's data keys of the three types, never a sealed part.
    CONSTRAINT ck_calling_notification_publications_envelope CHECK (envelope IS NULL OR (
        jsonb_typeof(envelope) = 'object'
        AND octet_length(envelope::text) <= 4096
        AND NOT (envelope ? 'sealed')
        AND jsonb_typeof(envelope -> 'data') = 'object'
        AND (envelope -> 'data') - ARRAY['callId', 'callKind', 'voiceMessageId', 'recordingId'] = '{}'::jsonb
        AND envelope ->> 'eventId' = event_id
        AND envelope ->> 'eventType' = event_type
        AND envelope -> 'data' ->> 'callId' = call_id::text
        AND (envelope -> 'scope' ->> 'tenantId') = tenant_id::text
        AND (envelope -> 'scope' ->> 'applicationId') = application_id::text
        AND (envelope -> 'scope' ->> 'environmentId') = environment_id::text))
);
CREATE INDEX idx_calling_notification_publications_due
    ON calling_notification_publications (next_attempt_at) WHERE state = 'PENDING';
CREATE INDEX idx_calling_notification_publications_retention
    ON calling_notification_publications (completed_at) WHERE state <> 'PENDING';
CREATE INDEX idx_calling_notification_publications_call
    ON calling_notification_publications (call_id);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    /// Only for the destructive development / test subcommands (`down`, `fresh`, `refresh`,
    /// `reset`), which the SEC-015 guard refuses in production: the main history's rollback
    /// drops `calls` and `platform_principals`, which this table references, so this history
    /// is rolled back first. An application rollback never runs it (data is retained).
    async fn down(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS calling_notification_publications;")
            .await?;
        Ok(())
    }
}
