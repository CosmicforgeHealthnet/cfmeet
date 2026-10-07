//! Calling's transactional outbox (Part C, Workstream D).
//!
//! Every call event is written in the same PostgreSQL transaction as the domain mutation
//! it describes, together with one publication-ready outbox row holding the event in the
//! `cfmeet.platform.event-envelope` format (payload: `cfmeet.calling.call-lifecycle-event`).
//! A call event cannot commit without its outbox row (deferred constraint trigger), so the
//! authoritative state, the immutable evidence and the publication record commit or roll
//! back together. Nothing is published before commit; a future publisher consumes PENDING
//! rows in `next_attempt_at` order. Envelopes are immutable; PENDING rows cannot be
//! deleted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_outbox_events (
    id              UUID PRIMARY KEY,
    event_id        VARCHAR(36)  NOT NULL,
    source_event_id UUID         NOT NULL,
    aggregate_type  TEXT         NOT NULL,
    aggregate_id    UUID         NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    event_type      VARCHAR(128) NOT NULL,
    schema_version  INTEGER      NOT NULL,
    envelope        JSONB        NOT NULL,
    status          TEXT         NOT NULL,
    attempt_count   INTEGER      NOT NULL,
    next_attempt_at TIMESTAMPTZ  NOT NULL,
    published_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ  NOT NULL,
    updated_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT fk_calling_outbox_events_source
        FOREIGN KEY (source_event_id) REFERENCES call_events (event_id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_outbox_events_call
        FOREIGN KEY (tenant_id, application_id, environment_id, aggregate_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_calling_outbox_events_event_id UNIQUE (event_id),
    CONSTRAINT uq_calling_outbox_events_source UNIQUE (source_event_id),
    CONSTRAINT ck_calling_outbox_events_event_id CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_calling_outbox_events_aggregate_type CHECK (aggregate_type IN ('CALL')),
    CONSTRAINT ck_calling_outbox_events_event_type
        CHECK (event_type ~ '^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)+$'),
    CONSTRAINT ck_calling_outbox_events_schema_version CHECK (schema_version >= 1),
    CONSTRAINT ck_calling_outbox_events_status CHECK (status IN ('PENDING', 'PUBLISHED')),
    CONSTRAINT ck_calling_outbox_events_published CHECK ((status = 'PUBLISHED') = (published_at IS NOT NULL)),
    CONSTRAINT ck_calling_outbox_events_attempts CHECK (attempt_count >= 0),
    -- The stored envelope is the publication record of exactly this row.
    CONSTRAINT ck_calling_outbox_events_envelope CHECK (
        jsonb_typeof(envelope) = 'object'
        AND envelope ->> 'event_id' = event_id
        AND envelope ->> 'event_type' = event_type
        AND envelope -> 'schema_version' = to_jsonb(schema_version)
        AND envelope ->> 'producer_service' = 'cfmeet.calling'
        AND jsonb_typeof(envelope -> 'payload') = 'object'
        AND octet_length(envelope::text) <= 16384)
);
CREATE INDEX idx_calling_outbox_events_pending
    ON calling_outbox_events (next_attempt_at, created_at) WHERE status = 'PENDING';
CREATE INDEX idx_calling_outbox_events_aggregate ON calling_outbox_events (aggregate_id);
CREATE TRIGGER trg_calling_outbox_events_immutable
    BEFORE UPDATE ON calling_outbox_events
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'event_id', 'source_event_id', 'aggregate_type', 'aggregate_id', 'tenant_id',
        'application_id', 'environment_id', 'event_type', 'schema_version', 'envelope', 'created_at');
CREATE TRIGGER trg_calling_outbox_events_set_once
    BEFORE UPDATE ON calling_outbox_events
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('published_at');
CREATE TRIGGER trg_calling_outbox_events_status_transition
    BEFORE UPDATE ON calling_outbox_events
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status', 'PENDING>PUBLISHED');

-- Unpublished events are never discarded.
CREATE FUNCTION calling_outbox_events_guard_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'PENDING' THEN
        RAISE EXCEPTION 'calling invariant violated: pending outbox event % cannot be deleted', OLD.event_id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN OLD;
END;
$$;
CREATE TRIGGER trg_calling_outbox_events_guard_delete
    BEFORE DELETE ON calling_outbox_events
    FOR EACH ROW EXECUTE FUNCTION calling_outbox_events_guard_delete();

-- A call event cannot commit without its publication-ready outbox row.
CREATE FUNCTION calling_call_events_require_outbox() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM calling_outbox_events o WHERE o.source_event_id = NEW.event_id) THEN
        RAISE EXCEPTION 'calling invariant violated: call event % has no outbox event', NEW.event_id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER trg_call_events_require_outbox
    AFTER INSERT ON call_events
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION calling_call_events_require_outbox();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_call_events_require_outbox ON call_events;
DROP FUNCTION IF EXISTS calling_call_events_require_outbox();
DROP TABLE IF EXISTS calling_outbox_events;
DROP FUNCTION IF EXISTS calling_outbox_events_guard_delete();
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
