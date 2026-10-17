//! Workstream G: durable provider commands, attempt evidence, authenticated provider
//! events and operational incidents.
//!
//! * `number_provider_commands` — the durable intent of every provider-affecting
//!   operation, written in the same transaction as the local state it implies. One status
//!   edge set (`number_provider_command_transition_allowed`); an UNKNOWN outcome leaves only
//!   through reconciliation; at most one lifecycle command in flight per resource.
//! * `number_provider_command_attempts` — append-only transmission / response evidence.
//! * `number_provider_events` — append-only, signature-verified, de-duplicated provider
//!   callbacks with their disposition (applied, stale, unmatched, conflict, …).
//! * `number_provider_inventory_refs` — which provider object each Number Resource is
//!   (internal, append-only), so search results and callbacks map to one canonical id.
//! * `numbering_incidents` — de-duplicated operational incidents (drift, unknown outcomes,
//!   callback authentication failures) for operators; never customer data.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261019_000005_provider_commands"
    }
}

const UP: &str = r#"
-- Which provider object a Number Resource is (internal; before and after any binding).
CREATE TABLE number_provider_inventory_refs (
    id                 UUID         NOT NULL,
    number_resource_id UUID         NOT NULL,
    provider_id        VARCHAR(64)  NOT NULL,
    external_ref       VARCHAR(200) NOT NULL,
    first_seen_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_provider_inventory_refs PRIMARY KEY (id),
    CONSTRAINT uq_number_provider_inventory_refs_ref UNIQUE (provider_id, external_ref),
    CONSTRAINT uq_number_provider_inventory_refs_resource UNIQUE (number_resource_id, provider_id),
    CONSTRAINT fk_number_provider_inventory_refs_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT ck_number_provider_inventory_refs_provider CHECK (provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_provider_inventory_refs_ref CHECK (length(external_ref) BETWEEN 1 AND 200)
);
CREATE TRIGGER trg_number_provider_inventory_refs_append_only
    BEFORE UPDATE OR DELETE ON number_provider_inventory_refs
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

CREATE FUNCTION number_provider_command_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('PENDING', 'DISPATCHING'), ('PENDING', 'FAILED'), ('PENDING', 'UNSUPPORTED'),
        ('DISPATCHING', 'SUCCEEDED'), ('DISPATCHING', 'FAILED'), ('DISPATCHING', 'UNSUPPORTED'),
        ('DISPATCHING', 'UNKNOWN'), ('DISPATCHING', 'PENDING'),
        ('UNKNOWN', 'RECONCILED_SUCCEEDED'), ('UNKNOWN', 'RECONCILED_FAILED')
    )
$$;

CREATE TABLE number_provider_commands (
    id                 UUID         NOT NULL,
    provider_id        VARCHAR(64)  NOT NULL,
    operation          VARCHAR(24)  NOT NULL,
    number_resource_id UUID         NOT NULL,
    reservation_id     UUID,
    idempotency_key    VARCHAR(160) NOT NULL,
    request            JSONB        NOT NULL,
    status             VARCHAR(24)  NOT NULL,
    attempts           INTEGER      NOT NULL DEFAULT 0,
    max_attempts       INTEGER      NOT NULL,
    job_id             UUID,
    external_ref       VARCHAR(200),
    result             JSONB,
    failure_code       VARCHAR(64),
    revision           BIGINT       NOT NULL DEFAULT 1,
    correlation_id     VARCHAR(64)  NOT NULL,
    created_at         TIMESTAMPTZ  NOT NULL,
    updated_at         TIMESTAMPTZ  NOT NULL,
    completed_at       TIMESTAMPTZ,
    CONSTRAINT pk_number_provider_commands PRIMARY KEY (id),
    CONSTRAINT uq_number_provider_commands_idempotency UNIQUE (provider_id, idempotency_key),
    CONSTRAINT fk_number_provider_commands_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_number_provider_commands_reservation FOREIGN KEY (reservation_id) REFERENCES number_reservations (id),
    CONSTRAINT ck_number_provider_commands_provider CHECK (provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_provider_commands_operation CHECK (operation IN ('RESERVE', 'PROVISION', 'CONFIGURE_VOICE',
        'CONFIGURE_SMS', 'SUSPEND', 'RESTORE', 'RELEASE', 'PORT_IN')),
    CONSTRAINT ck_number_provider_commands_status CHECK (status IN ('PENDING', 'DISPATCHING', 'SUCCEEDED', 'FAILED',
        'UNSUPPORTED', 'UNKNOWN', 'RECONCILED_SUCCEEDED', 'RECONCILED_FAILED')),
    CONSTRAINT ck_number_provider_commands_reservation CHECK ((operation = 'RESERVE') = (reservation_id IS NOT NULL)),
    CONSTRAINT ck_number_provider_commands_attempts CHECK (attempts >= 0 AND max_attempts BETWEEN 1 AND 20
        AND attempts <= max_attempts),
    CONSTRAINT ck_number_provider_commands_key CHECK (idempotency_key ~ '^[A-Za-z0-9._:-]{8,160}$'),
    -- Commands carry canonical requests only: never a credential or secret.
    CONSTRAINT ck_number_provider_commands_request CHECK (jsonb_typeof(request) = 'object'
        AND request::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|signature)[a-z_]*":'),
    CONSTRAINT ck_number_provider_commands_result CHECK (result IS NULL OR (jsonb_typeof(result) = 'object'
        AND result::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|signature)[a-z_]*":')),
    CONSTRAINT ck_number_provider_commands_failure CHECK ((status IN ('FAILED', 'RECONCILED_FAILED', 'UNSUPPORTED'))
        = (failure_code IS NOT NULL)),
    CONSTRAINT ck_number_provider_commands_completed CHECK ((status IN ('SUCCEEDED', 'FAILED', 'UNSUPPORTED',
        'RECONCILED_SUCCEEDED', 'RECONCILED_FAILED')) = (completed_at IS NOT NULL)),
    CONSTRAINT ck_number_provider_commands_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_number_provider_commands_revision CHECK (revision >= 1)
);
-- At most one lifecycle-affecting command in flight per number (no competing authority).
CREATE UNIQUE INDEX uq_number_provider_commands_in_flight ON number_provider_commands (number_resource_id)
    WHERE status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')
      AND operation IN ('RESERVE', 'PROVISION', 'SUSPEND', 'RESTORE', 'RELEASE', 'PORT_IN');
CREATE UNIQUE INDEX uq_number_provider_commands_config_in_flight ON number_provider_commands (number_resource_id, operation)
    WHERE status IN ('PENDING', 'DISPATCHING', 'UNKNOWN') AND operation IN ('CONFIGURE_VOICE', 'CONFIGURE_SMS');
CREATE INDEX ix_number_provider_commands_unknown ON number_provider_commands (updated_at) WHERE status = 'UNKNOWN';

CREATE FUNCTION number_guard_provider_command() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'provider commands are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'PENDING' OR NEW.revision <> 1 OR NEW.attempts <> 0 THEN
            RAISE EXCEPTION 'a provider command starts PENDING at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status', 'attempts', 'job_id', 'external_ref', 'result', 'failure_code', 'revision',
            'updated_at', 'completed_at'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status', 'attempts', 'job_id', 'external_ref', 'result', 'failure_code', 'revision',
            'updated_at', 'completed_at']) THEN
        RAISE EXCEPTION 'the intent of a provider command is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'provider command updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.attempts < OLD.attempts THEN
        RAISE EXCEPTION 'attempts never decrease' USING ERRCODE = '23514';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status
       AND NOT number_provider_command_transition_allowed(OLD.status, NEW.status) THEN
        RAISE EXCEPTION 'provider command transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
    END IF;
    IF OLD.status IN ('SUCCEEDED', 'FAILED', 'UNSUPPORTED', 'RECONCILED_SUCCEEDED', 'RECONCILED_FAILED') THEN
        RAISE EXCEPTION 'a completed provider command is final' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_provider_commands_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_provider_commands
    FOR EACH ROW EXECUTE FUNCTION number_guard_provider_command();
CREATE TRIGGER trg_number_provider_commands_no_truncate
    BEFORE TRUNCATE ON number_provider_commands
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

CREATE TABLE number_provider_command_attempts (
    id                   UUID         NOT NULL,
    command_id           UUID         NOT NULL,
    attempt              INTEGER      NOT NULL,
    outcome              VARCHAR(24)  NOT NULL,
    transmitted          BOOLEAN      NOT NULL,
    provider_request_ref VARCHAR(200),
    response_code        VARCHAR(64),
    response             JSONB        NOT NULL DEFAULT '{}'::jsonb,
    started_at           TIMESTAMPTZ  NOT NULL,
    finished_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_provider_command_attempts PRIMARY KEY (id),
    CONSTRAINT uq_number_provider_command_attempts UNIQUE (command_id, attempt),
    CONSTRAINT fk_number_provider_command_attempts_command FOREIGN KEY (command_id) REFERENCES number_provider_commands (id),
    CONSTRAINT ck_number_provider_command_attempts_outcome CHECK (outcome IN ('SUCCEEDED', 'FAILED', 'UNSUPPORTED',
        'UNKNOWN', 'NOT_TRANSMITTED', 'READ_BACK')),
    -- Not transmitted means nothing reached the provider; unknown means it may have.
    CONSTRAINT ck_number_provider_command_attempts_transmitted CHECK ((outcome = 'NOT_TRANSMITTED') = (NOT transmitted)),
    CONSTRAINT ck_number_provider_command_attempts_response CHECK (jsonb_typeof(response) = 'object'
        AND response::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|signature)[a-z_]*":'),
    CONSTRAINT ck_number_provider_command_attempts_time CHECK (finished_at >= started_at)
);
CREATE TRIGGER trg_number_provider_command_attempts_append_only
    BEFORE UPDATE OR DELETE ON number_provider_command_attempts
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_provider_command_attempts_no_truncate
    BEFORE TRUNCATE ON number_provider_command_attempts
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

CREATE TABLE number_provider_events (
    id                 UUID         NOT NULL,
    provider_id        VARCHAR(64)  NOT NULL,
    provider_event_id  VARCHAR(128) NOT NULL,
    event_kind         VARCHAR(32)  NOT NULL,
    external_ref       VARCHAR(200),
    number_resource_id UUID,
    command_id         UUID,
    provider_sequence  BIGINT,
    occurred_at        TIMESTAMPTZ  NOT NULL,
    received_at        TIMESTAMPTZ  NOT NULL,
    payload_sha256     CHAR(64)     NOT NULL,
    normalized         JSONB        NOT NULL,
    disposition        VARCHAR(24)  NOT NULL,
    correlation_id     VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_provider_events PRIMARY KEY (id),
    -- Stable de-duplication key: a redelivered event is never stored or applied twice.
    CONSTRAINT uq_number_provider_events_dedupe UNIQUE (provider_id, provider_event_id),
    CONSTRAINT fk_number_provider_events_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_number_provider_events_command FOREIGN KEY (command_id) REFERENCES number_provider_commands (id),
    CONSTRAINT ck_number_provider_events_provider CHECK (provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_provider_events_kind CHECK (event_kind IN ('HOLD_CONFIRMED', 'HOLD_EXPIRED', 'PROVISIONED',
        'ROUTE_CONFIGURED', 'SUSPENDED', 'RESTORED', 'RELEASED', 'CAPABILITY_CHANGED', 'NUMBER_REVOKED')),
    CONSTRAINT ck_number_provider_events_disposition CHECK (disposition IN ('APPLIED', 'RECONCILE_REQUESTED',
        'CONFIRMED_EXISTING', 'STALE', 'UNMATCHED', 'CONFLICT', 'IGNORED_TERMINAL')),
    CONSTRAINT ck_number_provider_events_digest CHECK (payload_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_number_provider_events_normalized CHECK (jsonb_typeof(normalized) = 'object'
        AND normalized::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|signature|e164|phone)[a-z_]*":'),
    CONSTRAINT ck_number_provider_events_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE INDEX ix_number_provider_events_ordering ON number_provider_events (provider_id, external_ref, provider_sequence);
CREATE TRIGGER trg_number_provider_events_append_only
    BEFORE UPDATE OR DELETE ON number_provider_events
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_provider_events_no_truncate
    BEFORE TRUNCATE ON number_provider_events
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

CREATE TABLE numbering_incidents (
    id                 UUID         NOT NULL,
    dedupe_key         VARCHAR(200) NOT NULL,
    category           VARCHAR(40)  NOT NULL,
    severity           VARCHAR(16)  NOT NULL,
    status             VARCHAR(16)  NOT NULL,
    provider_id        VARCHAR(64),
    number_resource_id UUID,
    command_id         UUID,
    occurrences        INTEGER      NOT NULL DEFAULT 1,
    detail             JSONB        NOT NULL,
    first_seen_at      TIMESTAMPTZ  NOT NULL,
    last_seen_at       TIMESTAMPTZ  NOT NULL,
    resolved_at        TIMESTAMPTZ,
    resolved_by        VARCHAR(200),
    CONSTRAINT pk_numbering_incidents PRIMARY KEY (id),
    CONSTRAINT fk_numbering_incidents_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_numbering_incidents_command FOREIGN KEY (command_id) REFERENCES number_provider_commands (id),
    CONSTRAINT ck_numbering_incidents_category CHECK (category IN ('PROVIDER_STATE_DRIFT', 'UNKNOWN_PROVIDER_OUTCOME',
        'CALLBACK_AUTHENTICATION_FAILED', 'UNMATCHED_PROVIDER_EVENT', 'PROVIDER_RELEASED_UNEXPECTEDLY',
        'RECONCILIATION_CONFLICT', 'PROVIDER_OUTAGE')),
    CONSTRAINT ck_numbering_incidents_severity CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    CONSTRAINT ck_numbering_incidents_status CHECK (status IN ('OPEN', 'RESOLVED')),
    CONSTRAINT ck_numbering_incidents_resolved CHECK ((status = 'RESOLVED') = (resolved_at IS NOT NULL AND resolved_by IS NOT NULL)),
    CONSTRAINT ck_numbering_incidents_detail CHECK (jsonb_typeof(detail) = 'object'
        AND detail::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|signature|e164|phone)[a-z_]*":'),
    CONSTRAINT ck_numbering_incidents_occurrences CHECK (occurrences >= 1)
);
-- One OPEN incident per dedupe key (repeats count occurrences).
CREATE UNIQUE INDEX uq_numbering_incidents_open ON numbering_incidents (dedupe_key) WHERE status = 'OPEN';
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_provider_commands WHERE status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')) THEN
        RAISE EXCEPTION 'provider commands are in flight or uncertain; reconcile them before rolling back';
    END IF;
END $$;
DROP TABLE IF EXISTS numbering_incidents;
DROP TABLE IF EXISTS number_provider_events;
DROP TABLE IF EXISTS number_provider_command_attempts;
DROP TABLE IF EXISTS number_provider_commands;
DROP TABLE IF EXISTS number_provider_inventory_refs;
DROP FUNCTION IF EXISTS number_guard_provider_command();
DROP FUNCTION IF EXISTS number_provider_command_transition_allowed(TEXT, TEXT);
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
