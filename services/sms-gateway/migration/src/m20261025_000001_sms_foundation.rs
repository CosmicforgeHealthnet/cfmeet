//! SMS Gateway foundation (Part IV Workstream M): the transport domain and its
//! reliability tables. Every table is `sms_`-prefixed and owned by this service only.
//!
//! * `sms_transport_threads` — transport grouping of the messages between ONE Number
//!   Resource and ONE remote party (by keyed fingerprint) in one environment. A thread is
//!   a transport index, not a conversation model.
//! * `sms_transports` — one message, inbound or outbound, with its state machine
//!   (REQUESTED, AUTHORIZED, SUBMITTING, SUBMITTED, DELIVERED, RECEIVED, FAILED, UNKNOWN,
//!   EXPIRED; transitions enforced by trigger). The remote number and the content are
//!   stored ONLY sealed (AEAD, `v2.<kid>.<…>`); the remote number is also kept as a keyed
//!   fingerprint (opt-out, threads, rate windows) and masked (`+***1234`).
//! * `sms_attempts` — submission attempts under a lease. At most ONE attempt of a
//!   transport may be live or possibly sent (CLAIMED / SUBMITTED / UNKNOWN; unique index):
//!   after an unknown outcome no further attempt can exist — never a blind resubmission.
//! * `sms_provider_bindings` — provider message reference → transport (set once).
//! * `sms_delivery_events` — every provider callback (delivery receipt / inbound
//!   message), de-duplicated by `(provider_id, provider_event_ref)`, with its disposition.
//! * `sms_quarantined_inbound` — inbound messages that could not be attributed to a
//!   current owner (sealed, never delivered to a customer, purged after retention).
//! * `sms_usage_records` — estimated / confirmed cost per transport.
//! * `sms_optout_blocks` — recipients who must not receive messages (STOP, customer,
//!   abuse), by fingerprint.
//! * `sms_rate_windows` — fixed-window counters of the abuse / rate controls.
//! * `sms_outbox_events` — the transactional outbox (lease, bounded retry, DEAD_LETTER).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------------------
-- Shared guards
-- ---------------------------------------------------------------------------------------
-- A sealed value: `v2.<kid>.<base64url>` (never a plaintext number or text).
CREATE FUNCTION sms_is_sealed(value TEXT) RETURNS BOOLEAN LANGUAGE sql IMMUTABLE AS $$
    SELECT value ~ '^v2\.[A-Za-z0-9_-]{1,64}\.[A-Za-z0-9_-]{20,}$'
$$;

CREATE TABLE sms_transport_threads (
    id                 UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    number_resource_id UUID         NOT NULL,
    remote_fingerprint CHAR(64)     NOT NULL,
    remote_masked      VARCHAR(20)  NOT NULL,
    message_count      BIGINT       NOT NULL DEFAULT 0,
    last_message_at    TIMESTAMPTZ  NOT NULL,
    created_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_sms_transport_threads PRIMARY KEY (id),
    CONSTRAINT uq_sms_transport_threads_party UNIQUE (environment_id, number_resource_id, remote_fingerprint),
    CONSTRAINT uq_sms_transport_threads_scope UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_sms_transport_threads_fingerprint CHECK (remote_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_sms_transport_threads_masked CHECK (remote_masked ~ '^\+\*{3}[0-9]{0,4}$'),
    CONSTRAINT ck_sms_transport_threads_count CHECK (message_count >= 0)
);
CREATE INDEX ix_sms_transport_threads_recent ON sms_transport_threads (environment_id, last_message_at DESC);

CREATE TABLE sms_transports (
    id                   UUID         NOT NULL,
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    direction            VARCHAR(8)   NOT NULL,
    status               VARCHAR(16)  NOT NULL,
    number_resource_id   UUID         NOT NULL,
    assignment_id        UUID,
    routing_revision     BIGINT,
    principal_id         UUID,
    target_kind          VARCHAR(40),
    target_ref           UUID,
    remote_sealed        TEXT         NOT NULL,
    remote_fingerprint   CHAR(64)     NOT NULL,
    remote_masked        VARCHAR(20)  NOT NULL,
    remote_country       CHAR(2),
    -- Outbound: the sender number Numbering released for THIS message (sealed), set when
    -- the single-use SMS_SENDER grant was consumed.
    sender_sealed        TEXT,
    content_sealed       TEXT,
    content_fingerprint  CHAR(64)     NOT NULL,
    content_purged_at    TIMESTAMPTZ,
    content_encoding     VARCHAR(8)   NOT NULL,
    char_count           INTEGER      NOT NULL,
    segment_count        SMALLINT     NOT NULL,
    thread_id            UUID         NOT NULL,
    service_account_id   UUID,
    credential_id        UUID,
    idempotency_key      VARCHAR(128),
    request_digest       CHAR(64),
    sender_grant_id      UUID,
    inbound_grant_id     UUID,
    provider_id          VARCHAR(64),
    estimated_cost_minor BIGINT,
    confirmed_cost_minor BIGINT,
    cost_currency        CHAR(3),
    failure_code         VARCHAR(64),
    blocked_reason       VARCHAR(64),
    attempt_count        SMALLINT     NOT NULL DEFAULT 0,
    max_attempts         SMALLINT     NOT NULL DEFAULT 3,
    next_attempt_at      TIMESTAMPTZ,
    expires_at           TIMESTAMPTZ,
    submitted_at         TIMESTAMPTZ,
    delivered_at         TIMESTAMPTZ,
    received_at          TIMESTAMPTZ,
    failed_at            TIMESTAMPTZ,
    correlation_id       VARCHAR(64)  NOT NULL,
    version              BIGINT       NOT NULL DEFAULT 1,
    created_at           TIMESTAMPTZ  NOT NULL,
    updated_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_sms_transports PRIMARY KEY (id),
    CONSTRAINT fk_sms_transports_thread FOREIGN KEY (tenant_id, application_id, environment_id, thread_id)
        REFERENCES sms_transport_threads (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_sms_transports_idempotency UNIQUE (environment_id, service_account_id, idempotency_key),
    CONSTRAINT ck_sms_transports_direction CHECK (direction IN ('OUTBOUND', 'INBOUND')),
    CONSTRAINT ck_sms_transports_status CHECK (status IN ('REQUESTED', 'AUTHORIZED', 'SUBMITTING', 'SUBMITTED',
        'DELIVERED', 'RECEIVED', 'FAILED', 'UNKNOWN', 'EXPIRED')),
    CONSTRAINT ck_sms_transports_inbound_status CHECK ((direction = 'INBOUND') = (status = 'RECEIVED')),
    CONSTRAINT ck_sms_transports_remote_sealed CHECK (sms_is_sealed(remote_sealed)),
    CONSTRAINT ck_sms_transports_sender_sealed CHECK (
        (sender_sealed IS NULL OR sms_is_sealed(sender_sealed))
        AND (direction = 'INBOUND' OR status IN ('REQUESTED', 'FAILED') OR sender_sealed IS NOT NULL)
        AND (direction = 'OUTBOUND' OR sender_sealed IS NULL)),
    CONSTRAINT ck_sms_transports_content_fingerprint CHECK (content_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_sms_transports_content_sealed CHECK (
        (content_sealed IS NULL OR sms_is_sealed(content_sealed))
        AND ((content_sealed IS NULL) = (content_purged_at IS NOT NULL))),
    CONSTRAINT ck_sms_transports_fingerprint CHECK (remote_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_sms_transports_masked CHECK (remote_masked ~ '^\+\*{3}[0-9]{0,4}$'),
    CONSTRAINT ck_sms_transports_country CHECK (remote_country IS NULL OR remote_country ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_sms_transports_encoding CHECK (content_encoding IN ('GSM7', 'UCS2')),
    CONSTRAINT ck_sms_transports_segments CHECK (segment_count BETWEEN 1 AND 10 AND char_count BETWEEN 1 AND 1600),
    CONSTRAINT ck_sms_transports_idempotency CHECK (
        (direction = 'OUTBOUND') = (idempotency_key IS NOT NULL AND request_digest IS NOT NULL)
        AND (idempotency_key IS NULL OR idempotency_key ~ '^[A-Za-z0-9._:-]{8,128}$')
        AND (request_digest IS NULL OR request_digest ~ '^[0-9a-f]{64}$')),
    CONSTRAINT ck_sms_transports_inbound_grant CHECK ((direction = 'INBOUND') = (inbound_grant_id IS NOT NULL)),
    CONSTRAINT ck_sms_transports_codes CHECK (
        (failure_code IS NULL OR failure_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
        AND (blocked_reason IS NULL OR blocked_reason ~ '^[A-Z][A-Z0-9_]{1,63}$')),
    CONSTRAINT ck_sms_transports_cost CHECK (
        (estimated_cost_minor IS NULL OR estimated_cost_minor >= 0)
        AND (confirmed_cost_minor IS NULL OR confirmed_cost_minor >= 0)
        AND (cost_currency IS NULL OR cost_currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_sms_transports_attempts CHECK (attempt_count BETWEEN 0 AND max_attempts AND max_attempts BETWEEN 1 AND 10),
    CONSTRAINT ck_sms_transports_failed CHECK ((status = 'FAILED') = (failed_at IS NOT NULL)
        AND (status <> 'FAILED' OR failure_code IS NOT NULL)),
    CONSTRAINT ck_sms_transports_target CHECK ((target_kind IS NULL) = (target_ref IS NULL)),
    CONSTRAINT ck_sms_transports_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE INDEX ix_sms_transports_list ON sms_transports (environment_id, created_at DESC, id);
CREATE INDEX ix_sms_transports_thread ON sms_transports (thread_id, created_at DESC);
CREATE INDEX ix_sms_transports_due ON sms_transports (next_attempt_at)
    WHERE status = 'AUTHORIZED';
CREATE INDEX ix_sms_transports_unknown ON sms_transports (updated_at) WHERE status = 'UNKNOWN';
CREATE INDEX ix_sms_transports_purge ON sms_transports (created_at) WHERE content_sealed IS NOT NULL;
CREATE INDEX ix_sms_transports_content ON sms_transports (environment_id, content_fingerprint, created_at)
    WHERE direction = 'OUTBOUND';

-- The transport state machine and its immutable facts.
CREATE FUNCTION sms_guard_transport() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    allowed BOOLEAN;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'sms_transports rows are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.direction = 'OUTBOUND' AND NEW.status NOT IN ('REQUESTED') THEN
            RAISE EXCEPTION 'an outbound transport starts REQUESTED' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
       OR NEW.application_id IS DISTINCT FROM OLD.application_id OR NEW.environment_id IS DISTINCT FROM OLD.environment_id
       OR NEW.direction IS DISTINCT FROM OLD.direction OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.remote_sealed IS DISTINCT FROM OLD.remote_sealed OR NEW.remote_fingerprint IS DISTINCT FROM OLD.remote_fingerprint
       OR NEW.remote_masked IS DISTINCT FROM OLD.remote_masked OR NEW.content_encoding IS DISTINCT FROM OLD.content_encoding
       OR NEW.content_fingerprint IS DISTINCT FROM OLD.content_fingerprint
       OR NEW.char_count IS DISTINCT FROM OLD.char_count OR NEW.segment_count IS DISTINCT FROM OLD.segment_count
       OR NEW.thread_id IS DISTINCT FROM OLD.thread_id OR NEW.idempotency_key IS DISTINCT FROM OLD.idempotency_key
       OR NEW.request_digest IS DISTINCT FROM OLD.request_digest OR NEW.service_account_id IS DISTINCT FROM OLD.service_account_id
       OR NEW.inbound_grant_id IS DISTINCT FROM OLD.inbound_grant_id OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id THEN
        RAISE EXCEPTION 'sms transport facts are immutable' USING ERRCODE = '42501';
    END IF;
    -- Content changes only by the retention purge (sealed -> NULL).
    IF NEW.content_sealed IS DISTINCT FROM OLD.content_sealed AND NOT (NEW.content_sealed IS NULL) THEN
        RAISE EXCEPTION 'sms content is never rewritten' USING ERRCODE = '42501';
    END IF;
    -- Set-once references.
    IF (OLD.sender_grant_id IS NOT NULL AND NEW.sender_grant_id IS DISTINCT FROM OLD.sender_grant_id)
       OR (OLD.sender_sealed IS NOT NULL AND NEW.sender_sealed IS DISTINCT FROM OLD.sender_sealed)
       OR (OLD.provider_id IS NOT NULL AND NEW.provider_id IS DISTINCT FROM OLD.provider_id)
       OR (OLD.assignment_id IS NOT NULL AND NEW.assignment_id IS DISTINCT FROM OLD.assignment_id) THEN
        RAISE EXCEPTION 'sms transport references are set once' USING ERRCODE = '42501';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status THEN
        allowed := CASE OLD.status
            WHEN 'REQUESTED'  THEN NEW.status IN ('AUTHORIZED', 'FAILED', 'EXPIRED')
            WHEN 'AUTHORIZED' THEN NEW.status IN ('SUBMITTING', 'FAILED', 'EXPIRED')
            -- A definitely-unsent attempt returns the transport to AUTHORIZED (retry).
            WHEN 'SUBMITTING' THEN NEW.status IN ('SUBMITTED', 'FAILED', 'UNKNOWN', 'AUTHORIZED')
            WHEN 'SUBMITTED'  THEN NEW.status IN ('DELIVERED', 'FAILED', 'UNKNOWN')
            -- Only provider evidence resolves an unknown outcome (never a resubmission).
            WHEN 'UNKNOWN'    THEN NEW.status IN ('SUBMITTED', 'DELIVERED', 'FAILED')
            ELSE FALSE
        END;
        IF NOT allowed THEN
            RAISE EXCEPTION 'sms transport transition % -> % is not allowed', OLD.status, NEW.status
                USING ERRCODE = '23514';
        END IF;
    END IF;
    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'sms transport version must advance by one' USING ERRCODE = '40001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_sms_transports_guard
    BEFORE INSERT OR UPDATE OR DELETE ON sms_transports
    FOR EACH ROW EXECUTE FUNCTION sms_guard_transport();

CREATE TABLE sms_attempts (
    id                   UUID         NOT NULL,
    transport_id         UUID         NOT NULL REFERENCES sms_transports (id) ON DELETE RESTRICT,
    attempt_number       SMALLINT     NOT NULL,
    status               VARCHAR(16)  NOT NULL,
    lease_owner          VARCHAR(100),
    lease_until          TIMESTAMPTZ,
    provider_id          VARCHAR(64)  NOT NULL,
    provider_message_ref VARCHAR(128),
    outcome_code         VARCHAR(64),
    started_at           TIMESTAMPTZ  NOT NULL,
    finished_at          TIMESTAMPTZ,
    CONSTRAINT pk_sms_attempts PRIMARY KEY (id),
    CONSTRAINT uq_sms_attempts_number UNIQUE (transport_id, attempt_number),
    CONSTRAINT ck_sms_attempts_status CHECK (status IN ('CLAIMED', 'SUBMITTED', 'NOT_SENT', 'REJECTED', 'UNKNOWN')),
    CONSTRAINT ck_sms_attempts_lease CHECK ((status = 'CLAIMED') = (lease_until IS NOT NULL)
        AND (lease_owner IS NULL) = (lease_until IS NULL)),
    CONSTRAINT ck_sms_attempts_ref CHECK ((status = 'SUBMITTED') <= (provider_message_ref IS NOT NULL)
        AND (provider_message_ref IS NULL OR provider_message_ref ~ '^[A-Za-z0-9._:-]{1,128}$')),
    CONSTRAINT ck_sms_attempts_number CHECK (attempt_number BETWEEN 1 AND 10),
    CONSTRAINT ck_sms_attempts_finished CHECK ((status = 'CLAIMED') = (finished_at IS NULL))
);
-- At most ONE attempt that is live or may have reached the provider: after an UNKNOWN or
-- SUBMITTED attempt, no new attempt can be inserted (never a blind resubmission).
CREATE UNIQUE INDEX uq_sms_attempts_live ON sms_attempts (transport_id)
    WHERE status IN ('CLAIMED', 'SUBMITTED', 'UNKNOWN');
CREATE INDEX ix_sms_attempts_leases ON sms_attempts (lease_until) WHERE status = 'CLAIMED';

CREATE FUNCTION sms_guard_attempt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'sms_attempts rows are never deleted' USING ERRCODE = '42501';
    END IF;
    IF NEW.transport_id IS DISTINCT FROM OLD.transport_id OR NEW.attempt_number IS DISTINCT FROM OLD.attempt_number
       OR NEW.provider_id IS DISTINCT FROM OLD.provider_id OR NEW.started_at IS DISTINCT FROM OLD.started_at THEN
        RAISE EXCEPTION 'sms attempt identity is immutable' USING ERRCODE = '42501';
    END IF;
    IF OLD.status IN ('NOT_SENT', 'REJECTED') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % attempt is final', OLD.status USING ERRCODE = '42501';
    END IF;
    -- An unknown attempt is resolved only to SUBMITTED by provider evidence.
    IF OLD.status = 'UNKNOWN' AND NEW.status NOT IN ('UNKNOWN', 'SUBMITTED') THEN
        RAISE EXCEPTION 'an UNKNOWN attempt is resolved by provider evidence only' USING ERRCODE = '42501';
    END IF;
    IF OLD.status = 'SUBMITTED' AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a SUBMITTED attempt is final' USING ERRCODE = '42501';
    END IF;
    IF OLD.provider_message_ref IS NOT NULL AND NEW.provider_message_ref IS DISTINCT FROM OLD.provider_message_ref THEN
        RAISE EXCEPTION 'the provider message reference is set once' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_sms_attempts_guard
    BEFORE UPDATE OR DELETE ON sms_attempts
    FOR EACH ROW EXECUTE FUNCTION sms_guard_attempt();

CREATE TABLE sms_provider_bindings (
    provider_id          VARCHAR(64)  NOT NULL,
    provider_message_ref VARCHAR(128) NOT NULL,
    transport_id         UUID         NOT NULL REFERENCES sms_transports (id) ON DELETE RESTRICT,
    attempt_id           UUID         REFERENCES sms_attempts (id) ON DELETE RESTRICT,
    created_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_sms_provider_bindings PRIMARY KEY (provider_id, provider_message_ref),
    CONSTRAINT uq_sms_provider_bindings_transport UNIQUE (transport_id),
    CONSTRAINT ck_sms_provider_bindings_ref CHECK (provider_message_ref ~ '^[A-Za-z0-9._:-]{1,128}$')
);
CREATE FUNCTION sms_forbid_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% rows are immutable', TG_TABLE_NAME USING ERRCODE = '42501';
END $$;
CREATE TRIGGER trg_sms_provider_bindings_immutable
    BEFORE UPDATE OR DELETE ON sms_provider_bindings
    FOR EACH ROW EXECUTE FUNCTION sms_forbid_change();

CREATE TABLE sms_delivery_events (
    id                 UUID         NOT NULL,
    provider_id        VARCHAR(64)  NOT NULL,
    provider_event_ref VARCHAR(128) NOT NULL,
    kind               VARCHAR(24)  NOT NULL,
    transport_id       UUID         REFERENCES sms_transports (id) ON DELETE RESTRICT,
    normalized_status  VARCHAR(16),
    disposition        VARCHAR(16)  NOT NULL,
    reason             VARCHAR(64),
    occurred_at        TIMESTAMPTZ,
    received_at        TIMESTAMPTZ  NOT NULL,
    body_digest        CHAR(64)     NOT NULL,
    CONSTRAINT pk_sms_delivery_events PRIMARY KEY (id),
    CONSTRAINT uq_sms_delivery_events_ref UNIQUE (provider_id, provider_event_ref),
    CONSTRAINT ck_sms_delivery_events_kind CHECK (kind IN ('DELIVERY_RECEIPT', 'INBOUND_MESSAGE')),
    CONSTRAINT ck_sms_delivery_events_status CHECK (normalized_status IS NULL
        OR normalized_status IN ('ACCEPTED', 'DELIVERED', 'FAILED', 'EXPIRED', 'UNKNOWN', 'RECEIVED')),
    CONSTRAINT ck_sms_delivery_events_disposition CHECK (disposition IN
        ('APPLIED', 'STALE', 'UNMATCHED', 'QUARANTINED', 'REFUSED')),
    CONSTRAINT ck_sms_delivery_events_ref CHECK (provider_event_ref ~ '^[A-Za-z0-9._:-]{1,128}$'),
    CONSTRAINT ck_sms_delivery_events_digest CHECK (body_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_sms_delivery_events_reason CHECK (reason IS NULL OR reason ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
CREATE INDEX ix_sms_delivery_events_transport ON sms_delivery_events (transport_id, received_at);
CREATE TRIGGER trg_sms_delivery_events_immutable
    BEFORE UPDATE OR DELETE ON sms_delivery_events
    FOR EACH ROW EXECUTE FUNCTION sms_forbid_change();

CREATE TABLE sms_quarantined_inbound (
    id                 UUID         NOT NULL,
    provider_id        VARCHAR(64)  NOT NULL,
    delivery_event_id  UUID         NOT NULL REFERENCES sms_delivery_events (id) ON DELETE RESTRICT,
    number_fingerprint CHAR(64)     NOT NULL,
    reason             VARCHAR(64)  NOT NULL,
    content_sealed     TEXT,
    received_at        TIMESTAMPTZ  NOT NULL,
    purge_after        TIMESTAMPTZ  NOT NULL,
    purged_at          TIMESTAMPTZ,
    CONSTRAINT pk_sms_quarantined_inbound PRIMARY KEY (id),
    CONSTRAINT ck_sms_quarantined_inbound_sealed CHECK ((content_sealed IS NULL OR sms_is_sealed(content_sealed))
        AND ((content_sealed IS NULL) = (purged_at IS NOT NULL))),
    CONSTRAINT ck_sms_quarantined_inbound_fp CHECK (number_fingerprint ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_sms_quarantined_inbound_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,63}$')
);

CREATE TABLE sms_usage_records (
    id                   UUID         NOT NULL,
    transport_id         UUID         NOT NULL REFERENCES sms_transports (id) ON DELETE RESTRICT,
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    direction            VARCHAR(8)   NOT NULL,
    segments             SMALLINT     NOT NULL,
    estimated_cost_minor BIGINT       NOT NULL,
    confirmed_cost_minor BIGINT,
    currency             CHAR(3)      NOT NULL,
    status               VARCHAR(12)  NOT NULL,
    created_at           TIMESTAMPTZ  NOT NULL,
    updated_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_sms_usage_records PRIMARY KEY (id),
    CONSTRAINT uq_sms_usage_records_transport UNIQUE (transport_id),
    CONSTRAINT ck_sms_usage_records_direction CHECK (direction IN ('OUTBOUND', 'INBOUND')),
    CONSTRAINT ck_sms_usage_records_status CHECK (status IN ('ESTIMATED', 'CONFIRMED', 'VOIDED')),
    CONSTRAINT ck_sms_usage_records_confirmed CHECK ((status = 'CONFIRMED') = (confirmed_cost_minor IS NOT NULL)),
    CONSTRAINT ck_sms_usage_records_amounts CHECK (estimated_cost_minor >= 0 AND (confirmed_cost_minor IS NULL OR confirmed_cost_minor >= 0)
        AND segments BETWEEN 1 AND 10 AND currency ~ '^[A-Z]{3}$')
);
CREATE INDEX ix_sms_usage_records_spend ON sms_usage_records (environment_id, created_at) WHERE status <> 'VOIDED';

CREATE TABLE sms_optout_blocks (
    id                 UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    number_resource_id UUID,
    remote_fingerprint CHAR(64)     NOT NULL,
    reason             VARCHAR(24)  NOT NULL,
    source_transport_id UUID        REFERENCES sms_transports (id) ON DELETE RESTRICT,
    created_at         TIMESTAMPTZ  NOT NULL,
    lifted_at          TIMESTAMPTZ,
    CONSTRAINT pk_sms_optout_blocks PRIMARY KEY (id),
    CONSTRAINT ck_sms_optout_blocks_reason CHECK (reason IN ('STOP_KEYWORD', 'CUSTOMER_REQUEST', 'ABUSE')),
    CONSTRAINT ck_sms_optout_blocks_fp CHECK (remote_fingerprint ~ '^[0-9a-f]{64}$')
);
CREATE UNIQUE INDEX uq_sms_optout_blocks_active ON sms_optout_blocks
    (environment_id, COALESCE(number_resource_id, '00000000-0000-0000-0000-000000000000'::uuid), remote_fingerprint)
    WHERE lifted_at IS NULL;

CREATE TABLE sms_rate_windows (
    subject_kind VARCHAR(32)  NOT NULL,
    subject_key  VARCHAR(200) NOT NULL,
    window_start TIMESTAMPTZ  NOT NULL,
    hits         INTEGER      NOT NULL,
    CONSTRAINT pk_sms_rate_windows PRIMARY KEY (subject_kind, subject_key, window_start),
    CONSTRAINT ck_sms_rate_windows_kind CHECK (subject_kind ~ '^[A-Z][A-Z_]{1,31}$'),
    CONSTRAINT ck_sms_rate_windows_key CHECK (subject_key ~ '^[A-Za-z0-9._:-]{1,200}$'),
    CONSTRAINT ck_sms_rate_windows_hits CHECK (hits >= 1)
);

CREATE TABLE sms_outbox_events (
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
    CONSTRAINT pk_sms_outbox_events PRIMARY KEY (id),
    CONSTRAINT uq_sms_outbox_events_sequence UNIQUE (sequence),
    CONSTRAINT ck_sms_outbox_events_type CHECK (event_type ~ '^sms-gateway(\.[a-z][a-z0-9-]*){2,}$'
        AND length(event_type) <= 80),
    CONSTRAINT ck_sms_outbox_events_version CHECK (schema_version >= 1),
    CONSTRAINT ck_sms_outbox_events_aggregate CHECK (aggregate_type ~ '^[A-Z][A-Z_]{1,39}$'),
    CONSTRAINT ck_sms_outbox_events_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_sms_outbox_events_causation CHECK (causation_id IS NULL OR causation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    -- The payload never carries content, a number or a secret.
    CONSTRAINT ck_sms_outbox_events_payload CHECK (jsonb_typeof(payload) = 'object'
        AND payload::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|phone|msisdn|e164|content|body|text|grant)[a-z_]*":'
        AND payload::text !~ '\+[1-9][0-9]{6,14}'),
    CONSTRAINT ck_sms_outbox_events_status CHECK (status IN ('PENDING', 'PUBLISHED', 'DEAD_LETTER')),
    CONSTRAINT ck_sms_outbox_events_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_sms_outbox_events_published CHECK ((status = 'PUBLISHED') = (published_at IS NOT NULL)),
    CONSTRAINT ck_sms_outbox_events_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT ck_sms_outbox_events_scope CHECK (
        (tenant_id IS NULL AND application_id IS NULL AND environment_id IS NULL)
        OR (tenant_id IS NOT NULL AND application_id IS NOT NULL AND environment_id IS NOT NULL))
);
CREATE INDEX ix_sms_outbox_events_due ON sms_outbox_events (next_attempt_at, sequence) WHERE status = 'PENDING';
CREATE INDEX ix_sms_outbox_events_aggregate ON sms_outbox_events (aggregate_type, aggregate_id, sequence);

CREATE FUNCTION sms_guard_outbox() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'sms_outbox_events rows are never deleted' USING ERRCODE = '42501';
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.sequence IS DISTINCT FROM OLD.sequence
       OR NEW.event_type IS DISTINCT FROM OLD.event_type OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
       OR NEW.aggregate_type IS DISTINCT FROM OLD.aggregate_type OR NEW.aggregate_id IS DISTINCT FROM OLD.aggregate_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id
       OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id OR NEW.causation_id IS DISTINCT FROM OLD.causation_id
       OR NEW.payload IS DISTINCT FROM OLD.payload OR NEW.occurred_at IS DISTINCT FROM OLD.occurred_at THEN
        RAISE EXCEPTION 'sms outbox event content is immutable' USING ERRCODE = '42501';
    END IF;
    IF OLD.status <> 'PENDING' AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % outbox event is final', OLD.status USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_sms_outbox_events_guard
    BEFORE UPDATE OR DELETE ON sms_outbox_events
    FOR EACH ROW EXECUTE FUNCTION sms_guard_outbox();
CREATE TRIGGER trg_sms_outbox_events_no_truncate
    BEFORE TRUNCATE ON sms_outbox_events
    FOR EACH STATEMENT EXECUTE FUNCTION sms_guard_outbox();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS sms_outbox_events;
DROP FUNCTION IF EXISTS sms_guard_outbox();
DROP TABLE IF EXISTS sms_rate_windows;
DROP TABLE IF EXISTS sms_optout_blocks;
DROP TABLE IF EXISTS sms_usage_records;
DROP TABLE IF EXISTS sms_quarantined_inbound;
DROP TABLE IF EXISTS sms_delivery_events;
DROP TABLE IF EXISTS sms_provider_bindings;
DROP TABLE IF EXISTS sms_attempts;
DROP FUNCTION IF EXISTS sms_guard_attempt();
DROP TABLE IF EXISTS sms_transports;
DROP FUNCTION IF EXISTS sms_guard_transport();
DROP TABLE IF EXISTS sms_transport_threads;
DROP FUNCTION IF EXISTS sms_forbid_change();
DROP FUNCTION IF EXISTS sms_is_sealed(TEXT);
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
