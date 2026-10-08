//! Durable commands, provider events and the transactional outbox (Part E §34, §36, §48,
//! §49, §166–§168).
//!
//! * `telephony_commands`: every provider-facing action is a durable command executed by
//!   lease-holding workers (`FOR UPDATE SKIP LOCKED`). `transmitted_at` is the fence:
//!   a command whose provider request may have left the process is never blindly
//!   retried — it becomes UNKNOWN_OUTCOME and is reconciled. A command id and a
//!   (requester, idempotency key) pair each have exactly one row, hence at most one
//!   provider effect. The short-lived Contact Resolution Grant secret is held only until
//!   the command resolves it (cleared on use, failure or expiry).
//! * `provider_events`: provider-qualified, deduplicated (`provider_id`,
//!   `provider_event_id`) evidence of normalized provider facts, with a bounded,
//!   restricted, redacted detail and a retention bound. Unknown provider objects are
//!   QUARANTINED, never attached to an arbitrary leg.
//! * `telephony_outbox`: normalized events for Calling, written in the same transaction
//!   as the state they describe, with a gap-free per-leg sequence.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_commands (
    id                UUID PRIMARY KEY,
    kind              TEXT         NOT NULL,
    telephony_leg_id  UUID REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    bridge_id         UUID,
    requester_service VARCHAR(64)  NOT NULL,
    idempotency_key   VARCHAR(128) NOT NULL,
    request_digest    CHAR(64)     NOT NULL,
    status            TEXT         NOT NULL,
    attempts          INTEGER      NOT NULL DEFAULT 0,
    max_attempts      INTEGER      NOT NULL,
    next_attempt_at   TIMESTAMPTZ  NOT NULL,
    lease_owner       VARCHAR(64),
    lease_until       TIMESTAMPTZ,
    transmitted_at    TIMESTAMPTZ,
    grant_id          UUID,
    grant_secret      TEXT,
    grant_expires_at  TIMESTAMPTZ,
    payload           JSONB        NOT NULL,
    result_code       VARCHAR(64),
    failure_category  TEXT,
    request_id        VARCHAR(64)  NOT NULL,
    correlation_id    VARCHAR(64)  NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL,
    updated_at        TIMESTAMPTZ  NOT NULL,
    finished_at       TIMESTAMPTZ,
    CONSTRAINT uq_telephony_commands_idempotency UNIQUE (requester_service, idempotency_key),
    CONSTRAINT ck_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'SEND_DTMF', 'QUERY_STATUS')),
    CONSTRAINT ck_telephony_commands_status CHECK (status IN (
        'PENDING', 'CLAIMED', 'RUNNING', 'SUCCEEDED', 'FAILED', 'UNKNOWN_OUTCOME', 'RECONCILING',
        'DEAD_LETTER')),
    CONSTRAINT ck_telephony_commands_target CHECK (telephony_leg_id IS NOT NULL OR bridge_id IS NOT NULL),
    CONSTRAINT ck_telephony_commands_key CHECK (idempotency_key ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_telephony_commands_attempts CHECK (attempts >= 0 AND max_attempts BETWEEN 1 AND 20),
    CONSTRAINT ck_telephony_commands_lease CHECK ((lease_owner IS NULL) = (lease_until IS NULL)),
    CONSTRAINT ck_telephony_commands_grant CHECK (
        (grant_secret IS NULL OR (grant_id IS NOT NULL AND grant_expires_at IS NOT NULL))
        AND (grant_secret IS NULL OR status IN ('PENDING', 'CLAIMED', 'RUNNING'))),
    CONSTRAINT ck_telephony_commands_payload
        CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 8192),
    CONSTRAINT ck_telephony_commands_finished
        CHECK ((status IN ('SUCCEEDED', 'FAILED', 'DEAD_LETTER')) = (finished_at IS NOT NULL)),
    CONSTRAINT ck_telephony_commands_failure CHECK (failure_category IS NULL OR failure_category IN (
        'POLICY', 'TARGET_UNAVAILABLE', 'ROUTING', 'TIMEOUT', 'MEDIA', 'NETWORK', 'EXECUTION',
        'INTERNAL', 'UNKNOWN')),
    CONSTRAINT ck_telephony_commands_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_telephony_commands_due ON telephony_commands (status, next_attempt_at)
    WHERE status IN ('PENDING', 'CLAIMED', 'RUNNING', 'UNKNOWN_OUTCOME', 'RECONCILING');
CREATE INDEX idx_telephony_commands_leg ON telephony_commands (telephony_leg_id);
CREATE INDEX idx_telephony_commands_bridge ON telephony_commands (bridge_id);
CREATE TRIGGER trg_telephony_commands_immutable
    BEFORE UPDATE ON telephony_commands
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'kind', 'telephony_leg_id', 'bridge_id', 'requester_service', 'idempotency_key',
        'request_digest', 'payload', 'request_id', 'correlation_id', 'created_at', 'grant_id');
CREATE TRIGGER trg_telephony_commands_set_once
    BEFORE UPDATE ON telephony_commands
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment('transmitted_at', 'finished_at');
CREATE TRIGGER trg_telephony_commands_transition
    BEFORE UPDATE ON telephony_commands
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_transition('status',
        'PENDING>CLAIMED', 'PENDING>FAILED', 'PENDING>DEAD_LETTER',
        'CLAIMED>RUNNING', 'CLAIMED>PENDING', 'CLAIMED>FAILED', 'CLAIMED>SUCCEEDED', 'CLAIMED>DEAD_LETTER',
        'RUNNING>SUCCEEDED', 'RUNNING>FAILED', 'RUNNING>UNKNOWN_OUTCOME', 'RUNNING>PENDING',
        'RUNNING>DEAD_LETTER',
        'UNKNOWN_OUTCOME>RECONCILING', 'UNKNOWN_OUTCOME>SUCCEEDED', 'UNKNOWN_OUTCOME>FAILED',
        'RECONCILING>SUCCEEDED', 'RECONCILING>FAILED', 'RECONCILING>UNKNOWN_OUTCOME',
        'RECONCILING>DEAD_LETTER');
-- A transmitted command can never be put back to PENDING (no blind retry).
CREATE FUNCTION telephony_commands_guard_retry() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status = 'PENDING' AND OLD.status <> 'PENDING' AND OLD.transmitted_at IS NOT NULL THEN
        RAISE EXCEPTION 'telephony invariant violated: a transmitted command is never retried blindly'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_telephony_commands_no_blind_retry
    BEFORE UPDATE ON telephony_commands
    FOR EACH ROW EXECUTE FUNCTION telephony_commands_guard_retry();
CREATE TRIGGER trg_telephony_commands_no_delete
    BEFORE DELETE ON telephony_commands
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();

CREATE TABLE provider_events (
    id                  UUID PRIMARY KEY,
    provider_id         VARCHAR(32)  NOT NULL REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    provider_event_id   VARCHAR(160) NOT NULL,
    provider_call_ref   VARCHAR(160),
    telephony_leg_id    UUID REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    normalized_event    TEXT,
    disposition         TEXT         NOT NULL,
    provider_sequence   BIGINT,
    occurred_at         TIMESTAMPTZ  NOT NULL,
    received_at         TIMESTAMPTZ  NOT NULL,
    payload_digest      CHAR(64)     NOT NULL,
    restricted_detail   JSONB,
    retain_until        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT uq_provider_events_provider_event UNIQUE (provider_id, provider_event_id),
    CONSTRAINT ck_provider_events_event_id CHECK (provider_event_id ~ '^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,159}$'),
    CONSTRAINT ck_provider_events_normalized CHECK (normalized_event IS NULL OR normalized_event IN (
        'LEG_ACCEPTED', 'LEG_RINGING', 'LEG_ANSWERED', 'LEG_CONNECTED', 'LEG_HELD', 'LEG_RESUMED',
        'LEG_ENDED', 'LEG_FAILED', 'LEG_BUSY', 'LEG_NO_ANSWER', 'LEG_DECLINED', 'LEG_STATUS_UNKNOWN',
        'BRIDGE_ESTABLISHED', 'BRIDGE_ENDED', 'LEG_DTMF', 'INBOUND_OFFERED')),
    CONSTRAINT ck_provider_events_disposition CHECK (disposition IN (
        'APPLIED', 'STALE', 'LATE_AFTER_TERMINAL', 'QUARANTINED', 'IGNORED')),
    CONSTRAINT ck_provider_events_quarantine CHECK (disposition <> 'QUARANTINED' OR telephony_leg_id IS NULL),
    CONSTRAINT ck_provider_events_detail CHECK (restricted_detail IS NULL OR (
        jsonb_typeof(restricted_detail) = 'object' AND octet_length(restricted_detail::text) <= 2048)),
    CONSTRAINT ck_provider_events_retention CHECK (retain_until > received_at)
);
CREATE INDEX idx_provider_events_received ON provider_events (received_at);
CREATE INDEX idx_provider_events_leg ON provider_events (telephony_leg_id);
CREATE INDEX idx_provider_events_retention ON provider_events (retain_until);
CREATE INDEX idx_provider_events_quarantine ON provider_events (provider_id, provider_call_ref)
    WHERE disposition = 'QUARANTINED';
CREATE TRIGGER trg_provider_events_immutable
    BEFORE UPDATE ON provider_events
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'provider_id', 'provider_event_id', 'provider_call_ref', 'normalized_event',
        'provider_sequence', 'occurred_at', 'received_at', 'payload_digest', 'restricted_detail');

CREATE TABLE telephony_outbox (
    id               UUID PRIMARY KEY,
    telephony_leg_id UUID        REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    bridge_id        UUID,
    calling_call_id  UUID        NOT NULL,
    event_type       TEXT        NOT NULL,
    leg_sequence     BIGINT,
    payload          JSONB       NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    attempts         INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ NOT NULL,
    lease_until      TIMESTAMPTZ,
    delivered_at     TIMESTAMPTZ,
    last_error_code  VARCHAR(64),
    CONSTRAINT uq_telephony_outbox_leg_sequence UNIQUE (telephony_leg_id, leg_sequence),
    CONSTRAINT ck_telephony_outbox_event_type CHECK (event_type IN (
        'LEG_ACCEPTED', 'LEG_RINGING', 'LEG_ANSWERED', 'LEG_CONNECTED', 'LEG_HELD', 'LEG_RESUMED',
        'LEG_ENDED', 'LEG_FAILED', 'LEG_BUSY', 'LEG_NO_ANSWER', 'LEG_DECLINED', 'LEG_STATUS_UNKNOWN',
        'BRIDGE_ESTABLISHED', 'BRIDGE_ENDED', 'LEG_DTMF', 'USAGE_FINALIZED', 'RECONCILIATION_RESULT')),
    CONSTRAINT ck_telephony_outbox_target CHECK (
        (telephony_leg_id IS NOT NULL AND leg_sequence IS NOT NULL AND leg_sequence >= 1)
        OR (telephony_leg_id IS NULL AND leg_sequence IS NULL AND bridge_id IS NOT NULL)),
    CONSTRAINT ck_telephony_outbox_payload
        CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 4096),
    CONSTRAINT ck_telephony_outbox_attempts CHECK (attempts >= 0)
);
CREATE INDEX idx_telephony_outbox_pending ON telephony_outbox (next_attempt_at)
    WHERE delivered_at IS NULL;
CREATE TRIGGER trg_telephony_outbox_immutable
    BEFORE UPDATE ON telephony_outbox
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'telephony_leg_id', 'bridge_id', 'calling_call_id', 'event_type', 'leg_sequence',
        'payload', 'created_at');
CREATE TRIGGER trg_telephony_outbox_delivered_once
    BEFORE UPDATE ON telephony_outbox
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment('delivered_at');
CREATE TRIGGER trg_telephony_outbox_no_delete
    BEFORE DELETE ON telephony_outbox
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telephony_outbox;
DROP TABLE IF EXISTS provider_events;
DROP TABLE IF EXISTS telephony_commands;
DROP FUNCTION IF EXISTS telephony_commands_guard_retry();
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
