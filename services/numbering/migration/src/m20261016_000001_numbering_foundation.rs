//! Reliability foundations of Numbering (Part IV §C4), reusable by every later number
//! workflow:
//!
//! * `numbering_outbox_events`: the transactional outbox. A domain change and its events
//!   commit in ONE transaction; a publisher claims due events with a lease and a claim
//!   token (`FOR UPDATE SKIP LOCKED`), retries with bounded backoff and dead-letters
//!   exhausted events. Event content is immutable; rows are never deleted.
//! * `numbering_jobs`: a generic leased job queue. Every job declares its retry safety:
//!   IDEMPOTENT work whose lease expired is reclaimed; work that may have reached an
//!   external provider (RECONCILE_ON_UNCERTAINTY) is parked as UNKNOWN_OUTCOME instead of
//!   being repeated. Exhausted jobs are DEAD_LETTER. Claim tokens guard every completion.
//! * `numbering_idempotency_keys`: idempotency keys with scoped uniqueness (who × which
//!   operation × key) and the request fingerprint, so a replay returns the first outcome
//!   and a reused key with a different request is refused.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE numbering_outbox_events (
    id               UUID         NOT NULL,
    sequence         BIGSERIAL    NOT NULL,
    event_type       VARCHAR(80)  NOT NULL,
    schema_version   SMALLINT     NOT NULL DEFAULT 1,
    aggregate_type   VARCHAR(40)  NOT NULL,
    aggregate_id     UUID         NOT NULL,
    tenant_id        UUID,
    application_id   UUID,
    environment_id   UUID,
    correlation_id   VARCHAR(64)  NOT NULL,
    causation_id     VARCHAR(64),
    payload          JSONB        NOT NULL,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    status           VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_token      UUID,
    lease_expires_at TIMESTAMPTZ,
    last_error       VARCHAR(200),
    published_at     TIMESTAMPTZ,
    CONSTRAINT pk_numbering_outbox_events PRIMARY KEY (id),
    CONSTRAINT uq_numbering_outbox_events_sequence UNIQUE (sequence),
    CONSTRAINT ck_numbering_outbox_events_type CHECK (event_type ~ '^number(\.[a-z][a-z0-9-]*){2,}$'
        AND length(event_type) <= 80),
    CONSTRAINT ck_numbering_outbox_events_version CHECK (schema_version >= 1),
    CONSTRAINT ck_numbering_outbox_events_aggregate CHECK (aggregate_type ~ '^[A-Z][A-Z_]{1,39}$'),
    CONSTRAINT ck_numbering_outbox_events_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_numbering_outbox_events_causation CHECK (causation_id IS NULL OR causation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    -- The payload never carries a number, contact or secret (cfmeet.platform.event-envelope).
    CONSTRAINT ck_numbering_outbox_events_payload CHECK (jsonb_typeof(payload) = 'object'
        AND payload::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|phone|msisdn|e164)[a-z_]*":'),
    CONSTRAINT ck_numbering_outbox_events_status CHECK (status IN ('PENDING', 'PUBLISHED', 'DEAD_LETTER')),
    CONSTRAINT ck_numbering_outbox_events_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_numbering_outbox_events_published CHECK ((status = 'PUBLISHED') = (published_at IS NOT NULL)),
    CONSTRAINT ck_numbering_outbox_events_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT ck_numbering_outbox_events_scope CHECK (
        (tenant_id IS NULL AND application_id IS NULL AND environment_id IS NULL)
        OR (tenant_id IS NOT NULL AND application_id IS NOT NULL AND environment_id IS NOT NULL))
);
CREATE INDEX ix_numbering_outbox_events_due ON numbering_outbox_events (next_attempt_at, sequence)
    WHERE status = 'PENDING';
CREATE INDEX ix_numbering_outbox_events_aggregate ON numbering_outbox_events (aggregate_type, aggregate_id, sequence);

-- Event content is immutable; only the delivery bookkeeping changes; rows are never deleted.
CREATE FUNCTION numbering_guard_outbox() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'numbering_outbox_events rows are never deleted' USING ERRCODE = '42501';
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.sequence IS DISTINCT FROM OLD.sequence
       OR NEW.event_type IS DISTINCT FROM OLD.event_type OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
       OR NEW.aggregate_type IS DISTINCT FROM OLD.aggregate_type OR NEW.aggregate_id IS DISTINCT FROM OLD.aggregate_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id
       OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id OR NEW.causation_id IS DISTINCT FROM OLD.causation_id
       OR NEW.payload IS DISTINCT FROM OLD.payload OR NEW.occurred_at IS DISTINCT FROM OLD.occurred_at THEN
        RAISE EXCEPTION 'numbering outbox event content is immutable' USING ERRCODE = '42501';
    END IF;
    IF OLD.status <> 'PENDING' AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % outbox event is final', OLD.status USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_numbering_outbox_events_guard
    BEFORE UPDATE OR DELETE ON numbering_outbox_events
    FOR EACH ROW EXECUTE FUNCTION numbering_guard_outbox();
CREATE TRIGGER trg_numbering_outbox_events_no_truncate
    BEFORE TRUNCATE ON numbering_outbox_events
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_guard_outbox();

CREATE TABLE numbering_jobs (
    id               UUID         NOT NULL,
    kind             VARCHAR(64)  NOT NULL,
    dedupe_key       VARCHAR(200),
    payload          JSONB        NOT NULL DEFAULT '{}'::jsonb,
    retry_safety     VARCHAR(32)  NOT NULL,
    correlation_id   VARCHAR(64)  NOT NULL,
    causation_id     VARCHAR(64),
    status           VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER      NOT NULL DEFAULT 0,
    max_attempts     INTEGER      NOT NULL,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_token      UUID,
    lease_owner      VARCHAR(100),
    lease_expires_at TIMESTAMPTZ,
    last_error_code  VARCHAR(64),
    last_error       VARCHAR(200),
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_numbering_jobs PRIMARY KEY (id),
    CONSTRAINT uq_numbering_jobs_dedupe UNIQUE (kind, dedupe_key),
    CONSTRAINT ck_numbering_jobs_kind CHECK (kind ~ '^[a-z][a-z0-9_.]{1,63}$'),
    CONSTRAINT ck_numbering_jobs_payload CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT ck_numbering_jobs_retry_safety CHECK (retry_safety IN ('IDEMPOTENT', 'RECONCILE_ON_UNCERTAINTY')),
    CONSTRAINT ck_numbering_jobs_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_numbering_jobs_causation CHECK (causation_id IS NULL OR causation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_numbering_jobs_status CHECK (status IN
        ('PENDING', 'RUNNING', 'SUCCEEDED', 'FAILED', 'DEAD_LETTER', 'UNKNOWN_OUTCOME')),
    CONSTRAINT ck_numbering_jobs_attempts CHECK (attempts >= 0 AND attempts <= max_attempts),
    CONSTRAINT ck_numbering_jobs_max_attempts CHECK (max_attempts BETWEEN 1 AND 100),
    CONSTRAINT ck_numbering_jobs_lease CHECK (
        (status = 'RUNNING') = (lease_token IS NOT NULL)
        AND (lease_token IS NULL) = (lease_expires_at IS NULL)
        AND (lease_token IS NULL) = (lease_owner IS NULL)),
    CONSTRAINT ck_numbering_jobs_completed CHECK (
        (status IN ('SUCCEEDED', 'FAILED', 'DEAD_LETTER', 'UNKNOWN_OUTCOME')) = (completed_at IS NOT NULL))
);
CREATE INDEX ix_numbering_jobs_due ON numbering_jobs (kind, next_attempt_at) WHERE status = 'PENDING';
CREATE INDEX ix_numbering_jobs_leases ON numbering_jobs (lease_expires_at) WHERE status = 'RUNNING';

-- Finished jobs are final; the identity and safety of a job never change.
CREATE FUNCTION numbering_guard_jobs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'numbering_jobs rows are never deleted' USING ERRCODE = '42501';
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.dedupe_key IS DISTINCT FROM OLD.dedupe_key OR NEW.payload IS DISTINCT FROM OLD.payload
       OR NEW.retry_safety IS DISTINCT FROM OLD.retry_safety OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id
       OR NEW.causation_id IS DISTINCT FROM OLD.causation_id OR NEW.max_attempts IS DISTINCT FROM OLD.max_attempts
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'numbering job identity is immutable' USING ERRCODE = '42501';
    END IF;
    IF OLD.status IN ('SUCCEEDED', 'FAILED', 'DEAD_LETTER') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % job is final', OLD.status USING ERRCODE = '42501';
    END IF;
    -- An uncertain job is resolved only by reconciliation (to SUCCEEDED or FAILED).
    IF OLD.status = 'UNKNOWN_OUTCOME' AND NEW.status NOT IN ('UNKNOWN_OUTCOME', 'SUCCEEDED', 'FAILED') THEN
        RAISE EXCEPTION 'an UNKNOWN_OUTCOME job is resolved by reconciliation only' USING ERRCODE = '42501';
    END IF;
    IF NEW.attempts < OLD.attempts THEN
        RAISE EXCEPTION 'job attempts never decrease' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_numbering_jobs_guard
    BEFORE UPDATE OR DELETE ON numbering_jobs
    FOR EACH ROW EXECUTE FUNCTION numbering_guard_jobs();

CREATE TABLE numbering_idempotency_keys (
    id               UUID         NOT NULL,
    scope_kind       VARCHAR(16)  NOT NULL,
    scope_key        VARCHAR(200) NOT NULL,
    operation        VARCHAR(80)  NOT NULL,
    idempotency_key  VARCHAR(128) NOT NULL,
    request_hash     CHAR(64)     NOT NULL,
    status           VARCHAR(16)  NOT NULL,
    response_status  SMALLINT,
    response_body    JSONB,
    resource_id      UUID,
    created_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    expires_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_numbering_idempotency_keys PRIMARY KEY (id),
    CONSTRAINT uq_numbering_idempotency_keys_scope UNIQUE (scope_kind, scope_key, operation, idempotency_key),
    CONSTRAINT ck_numbering_idempotency_keys_scope_kind CHECK (scope_kind IN ('CUSTOMER', 'SERVICE', 'OPERATOR')),
    CONSTRAINT ck_numbering_idempotency_keys_scope_key CHECK (scope_key ~ '^[A-Za-z0-9._:@-]{1,200}$'),
    CONSTRAINT ck_numbering_idempotency_keys_operation CHECK (operation ~ '^[a-z][a-z0-9_.]{1,79}$'),
    CONSTRAINT ck_numbering_idempotency_keys_key CHECK (idempotency_key ~ '^[A-Za-z0-9._:-]{8,128}$'),
    CONSTRAINT ck_numbering_idempotency_keys_hash CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_numbering_idempotency_keys_status CHECK (status IN ('IN_PROGRESS', 'COMPLETED')),
    CONSTRAINT ck_numbering_idempotency_keys_completed CHECK (
        (status = 'COMPLETED') = (completed_at IS NOT NULL AND response_status IS NOT NULL)),
    CONSTRAINT ck_numbering_idempotency_keys_expiry CHECK (expires_at > created_at)
);
CREATE INDEX ix_numbering_idempotency_keys_expiry ON numbering_idempotency_keys (expires_at);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS numbering_idempotency_keys;
DROP TABLE IF EXISTS numbering_jobs;
DROP FUNCTION IF EXISTS numbering_guard_jobs();
DROP TABLE IF EXISTS numbering_outbox_events;
DROP FUNCTION IF EXISTS numbering_guard_outbox();
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
