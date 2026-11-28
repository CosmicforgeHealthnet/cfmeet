//! Events (ADR 0071; additive; the FIRST migration of the separate
//! `messaging_events_seaql_migrations` history, which no released image reads, so a rollback
//! to the previous image still boots on this schema).
//!
//! * `messaging_events` — one event (a single occurrence or a recurring series) with its
//!   scope (PERSONAL / CONVERSATION; ORGANIZATION and COMMUNITY are reserved), time, zone,
//!   recurrence, meeting link, cover media and state; `series_ends_at` (the last occurrence's
//!   end, NULL for an open series) bounds every query; `search_vector` serves search.
//! * `messaging_event_people` — organizer, co-organizers, invitees and conversation members
//!   who answered, with their RSVP.
//! * `messaging_event_reminders` — each person's own reminders with the next fire time and
//!   the scope of the session that set them (a reminder notice is written without a request).
//! * `messaging_event_notices` — the Notification Service outbox of invitations,
//!   cancellations and reminders (written only when enabled).
//! * The ADR 0069 media store accepts the purpose `EVENT` (cover images). The previous image
//!   never selects it and its sweep never removes a completed `EVENT` upload.
//!
//! No column holds message content of a conversation, a key or E2EE material. Principals,
//! tenants, applications and environments are opaque Platform Access ids.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_social_media DROP CONSTRAINT ck_messaging_social_media_purpose;
ALTER TABLE messaging_social_media ADD CONSTRAINT ck_messaging_social_media_purpose
    CHECK (purpose IN ('STATUS', 'CHANNEL', 'EVENT'));

CREATE TABLE messaging_events (
    id                     UUID          NOT NULL,
    tenant_id              UUID          NOT NULL,
    application_id         UUID          NOT NULL,
    environment_id         UUID          NOT NULL,
    organizer_principal_id UUID          NOT NULL,
    scope                  VARCHAR(12)   NOT NULL,
    conversation_id        UUID,
    title                  TEXT          NOT NULL,
    description            TEXT,
    location               TEXT,
    kind                   VARCHAR(8)    NOT NULL,
    meeting_id             VARCHAR(64),
    meeting_url            TEXT,
    cover_media_id         UUID,
    starts_at              TIMESTAMPTZ   NOT NULL,
    ends_at                TIMESTAMPTZ   NOT NULL,
    time_zone              VARCHAR(64)   NOT NULL,
    all_day                BOOLEAN       NOT NULL DEFAULT false,
    recurrence             JSONB,
    series_ends_at         TIMESTAMPTZ,
    state                  VARCHAR(9)    NOT NULL DEFAULT 'ACTIVE',
    cancelled_at           TIMESTAMPTZ,
    cancel_reason          TEXT,
    version                INTEGER       NOT NULL DEFAULT 1,
    created_at             TIMESTAMPTZ   NOT NULL,
    updated_at             TIMESTAMPTZ   NOT NULL,
    search_vector          TSVECTOR GENERATED ALWAYS AS (
        to_tsvector('simple', coalesce(title, '') || ' ' || coalesce(description, '') || ' ' || coalesce(location, ''))
    ) STORED,
    CONSTRAINT pk_messaging_events PRIMARY KEY (id),
    CONSTRAINT ck_messaging_events_scope CHECK (scope IN ('PERSONAL', 'CONVERSATION', 'ORGANIZATION', 'COMMUNITY')),
    CONSTRAINT ck_messaging_events_conversation CHECK ((scope = 'CONVERSATION') = (conversation_id IS NOT NULL)),
    CONSTRAINT ck_messaging_events_title CHECK (char_length(title) BETWEEN 1 AND 200),
    CONSTRAINT ck_messaging_events_description CHECK (description IS NULL OR char_length(description) <= 5000),
    CONSTRAINT ck_messaging_events_location CHECK (location IS NULL OR char_length(location) <= 500),
    CONSTRAINT ck_messaging_events_kind CHECK (kind IN ('ONLINE', 'PHYSICAL', 'CALL')),
    CONSTRAINT ck_messaging_events_meeting_url CHECK (meeting_url IS NULL OR (char_length(meeting_url) <= 2048 AND meeting_url LIKE 'https://%')),
    CONSTRAINT ck_messaging_events_time CHECK (ends_at > starts_at),
    CONSTRAINT ck_messaging_events_recurrence CHECK (recurrence IS NULL OR (jsonb_typeof(recurrence) = 'object' AND octet_length(recurrence::text) <= 512)),
    CONSTRAINT ck_messaging_events_state CHECK (state IN ('ACTIVE', 'CANCELLED')),
    CONSTRAINT ck_messaging_events_cancelled CHECK ((state = 'CANCELLED') = (cancelled_at IS NOT NULL)),
    CONSTRAINT ck_messaging_events_cancel_reason CHECK (cancel_reason IS NULL OR char_length(cancel_reason) <= 500)
);
CREATE INDEX ix_messaging_events_organizer ON messaging_events (environment_id, organizer_principal_id, starts_at);
CREATE INDEX ix_messaging_events_conversation ON messaging_events (environment_id, conversation_id, starts_at) WHERE conversation_id IS NOT NULL;
CREATE INDEX ix_messaging_events_series_end ON messaging_events (environment_id, series_ends_at);
CREATE INDEX ix_messaging_events_cover ON messaging_events (cover_media_id) WHERE cover_media_id IS NOT NULL;
CREATE INDEX ix_messaging_events_search ON messaging_events USING GIN (search_vector);

CREATE TABLE messaging_event_people (
    event_id      UUID         NOT NULL,
    principal_id  UUID         NOT NULL,
    role          VARCHAR(12)  NOT NULL,
    response      VARCHAR(9),
    responded_at  TIMESTAMPTZ,
    added_by      UUID         NOT NULL,
    added_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_event_people PRIMARY KEY (event_id, principal_id),
    CONSTRAINT fk_messaging_event_people_event FOREIGN KEY (event_id) REFERENCES messaging_events (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_event_people_role CHECK (role IN ('ORGANIZER', 'CO_ORGANIZER', 'INVITEE', 'MEMBER')),
    CONSTRAINT ck_messaging_event_people_response CHECK (response IS NULL OR response IN ('GOING', 'MAYBE', 'NOT_GOING')),
    CONSTRAINT ck_messaging_event_people_responded CHECK ((response IS NULL) = (responded_at IS NULL))
);
CREATE INDEX ix_messaging_event_people_principal ON messaging_event_people (principal_id, event_id);

CREATE TABLE messaging_event_reminders (
    event_id        UUID         NOT NULL,
    principal_id    UUID         NOT NULL,
    minutes_before  INTEGER      NOT NULL,
    next_fire_at    TIMESTAMPTZ,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    service_account_id  UUID         NOT NULL,
    environment_kind    VARCHAR(16)  NOT NULL,
    first_party         BOOLEAN      NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_event_reminders PRIMARY KEY (event_id, principal_id, minutes_before),
    CONSTRAINT fk_messaging_event_reminders_event FOREIGN KEY (event_id) REFERENCES messaging_events (id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_event_reminders_minutes CHECK (minutes_before BETWEEN 0 AND 40320)
);
CREATE INDEX ix_messaging_event_reminders_due ON messaging_event_reminders (next_fire_at) WHERE next_fire_at IS NOT NULL;
CREATE INDEX ix_messaging_event_reminders_principal ON messaging_event_reminders (principal_id, event_id);

CREATE TABLE messaging_event_notices (
    id                  UUID          NOT NULL,
    event_id            VARCHAR(64)   NOT NULL,
    kind                VARCHAR(9)    NOT NULL,
    dedupe_key          VARCHAR(160)  NOT NULL,
    tenant_id           UUID          NOT NULL,
    application_id      UUID          NOT NULL,
    environment_id      UUID          NOT NULL,
    subject_event_id    UUID          NOT NULL,
    principal_id        UUID          NOT NULL,
    occurrence_at       TIMESTAMPTZ,
    service_account_id  UUID          NOT NULL,
    environment_kind    VARCHAR(16)   NOT NULL,
    first_party         BOOLEAN       NOT NULL,
    envelope            JSONB,
    state               VARCHAR(8)    NOT NULL DEFAULT 'PENDING',
    outcome             VARCHAR(9),
    last_error          VARCHAR(64),
    attempts            INTEGER       NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ   NOT NULL,
    occurred_at         TIMESTAMPTZ   NOT NULL,
    created_at          TIMESTAMPTZ   NOT NULL,
    completed_at        TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_event_notices PRIMARY KEY (id),
    CONSTRAINT uq_messaging_event_notices_event UNIQUE (event_id),
    CONSTRAINT uq_messaging_event_notices_dedupe UNIQUE (dedupe_key),
    CONSTRAINT ck_messaging_event_notices_kind CHECK (kind IN ('INVITED', 'CANCELLED', 'REMINDER')),
    CONSTRAINT ck_messaging_event_notices_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_event_notices_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_messaging_event_notices_done CHECK ((state = 'PENDING') = (completed_at IS NULL))
);
CREATE INDEX ix_messaging_event_notices_due ON messaging_event_notices (next_attempt_at) WHERE state = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_event_notices;
DROP TABLE IF EXISTS messaging_event_reminders;
DROP TABLE IF EXISTS messaging_event_people;
DROP TABLE IF EXISTS messaging_events;
DELETE FROM messaging_social_media WHERE purpose = 'EVENT';
ALTER TABLE messaging_social_media DROP CONSTRAINT ck_messaging_social_media_purpose;
ALTER TABLE messaging_social_media ADD CONSTRAINT ck_messaging_social_media_purpose
    CHECK (purpose IN ('STATUS', 'CHANNEL'));
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
