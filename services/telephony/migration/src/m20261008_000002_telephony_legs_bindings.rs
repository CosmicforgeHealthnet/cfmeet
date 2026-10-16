//! Telephony legs and provider call bindings (Part E §32–§33, §48).
//!
//! * `telephony_legs`: the public-network EXECUTION of one canonical Calling CallLeg
//!   attempt (`calling_call_leg_id` + `generation`, opaque, never a foreign key). Its
//!   normalized execution lifecycle is distinct from the Call state. At most ONE
//!   non-terminal execution exists per CallLeg — UNKNOWN is non-terminal — so a second
//!   paid execution of the same leg cannot start while the first one's outcome is not
//!   established (duplicate-paid-call guard in the database).
//! * `provider_call_bindings`: the correlation between a leg and the provider's call
//!   object. The correlation key is persisted BEFORE any provider request; provider ids
//!   stay inside Telephony and are unique per provider.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE telephony_legs (
    id                     UUID PRIMARY KEY,
    calling_call_id        UUID        NOT NULL,
    calling_call_leg_id    UUID        NOT NULL,
    calling_attempt_id     UUID,
    tenant_id              UUID        NOT NULL,
    application_id         UUID        NOT NULL,
    environment_id         UUID        NOT NULL,
    direction              TEXT        NOT NULL,
    route_class            TEXT        NOT NULL,
    route_id               VARCHAR(64) REFERENCES telecom_routes (id) ON DELETE RESTRICT,
    provider_id            VARCHAR(32) REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    status                 TEXT        NOT NULL,
    generation             INTEGER     NOT NULL,
    destination_country    CHAR(2),
    contact_reference      UUID,
    relay_session_id       UUID,
    context_id             UUID,
    privacy_decision_id    UUID,
    call_authorization_id  UUID,
    inbound_resolution_id  UUID,
    spend_reservation_id   UUID,
    spend_reserved_minor   BIGINT,
    spend_currency         CHAR(3),
    provider_cost_minor    BIGINT,
    provider_cost_currency CHAR(3),
    max_duration_seconds   INTEGER     NOT NULL,
    media_attachment       TEXT        NOT NULL,
    end_reason             TEXT,
    failure_category       TEXT,
    usage_status           TEXT        NOT NULL,
    reconcile_after        TIMESTAMPTZ,
    reconcile_lease_until  TIMESTAMPTZ,
    reconcile_attempts     INTEGER     NOT NULL DEFAULT 0,
    last_provider_sequence BIGINT,
    event_sequence         BIGINT      NOT NULL DEFAULT 0,
    created_at             TIMESTAMPTZ NOT NULL,
    originating_at         TIMESTAMPTZ,
    ringing_at             TIMESTAMPTZ,
    answered_at            TIMESTAMPTZ,
    connected_at           TIMESTAMPTZ,
    ended_at               TIMESTAMPTZ,
    updated_at             TIMESTAMPTZ NOT NULL,
    version                BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT uq_telephony_legs_attempt UNIQUE (calling_call_leg_id, generation),
    CONSTRAINT ck_telephony_legs_direction CHECK (direction IN ('OUTBOUND', 'INBOUND')),
    CONSTRAINT ck_telephony_legs_route_class
        CHECK (route_class IN ('APP_TO_PSTN', 'PSTN_TO_APP', 'PSTN_RELAY', 'CALLBACK_RELAY')),
    CONSTRAINT ck_telephony_legs_status CHECK (status IN (
        'PLANNED', 'ORIGINATING', 'RINGING', 'ANSWERED', 'CONNECTED', 'HELD', 'ENDING',
        'ENDED', 'FAILED', 'UNKNOWN')),
    CONSTRAINT ck_telephony_legs_generation CHECK (generation BETWEEN 1 AND 64),
    CONSTRAINT ck_telephony_legs_country CHECK (destination_country IS NULL OR destination_country ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_telephony_legs_outbound_proof CHECK (direction = 'INBOUND' OR (
        calling_attempt_id IS NOT NULL AND call_authorization_id IS NOT NULL
        AND contact_reference IS NOT NULL AND relay_session_id IS NOT NULL
        AND privacy_decision_id IS NOT NULL AND spend_reservation_id IS NOT NULL
        AND spend_reserved_minor IS NOT NULL AND spend_currency IS NOT NULL)),
    CONSTRAINT ck_telephony_legs_inbound CHECK (direction = 'OUTBOUND' OR inbound_resolution_id IS NOT NULL),
    CONSTRAINT ck_telephony_legs_spend CHECK (
        (spend_reserved_minor IS NULL OR spend_reserved_minor >= 0)
        AND (spend_currency IS NULL OR spend_currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_telephony_legs_provider_cost CHECK (
        (provider_cost_minor IS NULL) = (provider_cost_currency IS NULL)
        AND (provider_cost_minor IS NULL OR provider_cost_minor >= 0)
        AND (provider_cost_currency IS NULL OR provider_cost_currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_telephony_legs_duration CHECK (max_duration_seconds BETWEEN 30 AND 86400),
    CONSTRAINT ck_telephony_legs_media CHECK (media_attachment IN ('NONE', 'APPLICATION')),
    CONSTRAINT ck_telephony_legs_end_reason CHECK (end_reason IS NULL OR end_reason IN (
        'COMPLETED', 'BUSY', 'NO_ANSWER', 'DECLINED', 'FAILED', 'CANCELLED', 'TIMEOUT',
        'DURATION_LIMIT', 'ORPHANED', 'NOT_SENT', 'ABSENT_AT_PROVIDER')),
    CONSTRAINT ck_telephony_legs_failure CHECK (failure_category IS NULL OR failure_category IN (
        'POLICY', 'TARGET_UNAVAILABLE', 'ROUTING', 'TIMEOUT', 'MEDIA', 'NETWORK', 'EXECUTION',
        'INTERNAL', 'UNKNOWN')),
    CONSTRAINT ck_telephony_legs_terminal
        CHECK ((status IN ('ENDED', 'FAILED')) = (ended_at IS NOT NULL AND end_reason IS NOT NULL)),
    CONSTRAINT ck_telephony_legs_failed CHECK ((status = 'FAILED') = (failure_category IS NOT NULL)),
    CONSTRAINT ck_telephony_legs_usage CHECK (usage_status IN ('PENDING', 'FINALIZED', 'NOT_APPLICABLE')),
    CONSTRAINT ck_telephony_legs_counters CHECK (reconcile_attempts >= 0 AND event_sequence >= 0 AND version >= 1),
    CONSTRAINT ck_telephony_legs_timestamps CHECK (
        (connected_at IS NULL OR answered_at IS NOT NULL)
        AND (answered_at IS NULL OR answered_at >= created_at)
        AND (ended_at IS NULL OR ended_at >= created_at)
        AND updated_at >= created_at)
);
-- At most one non-terminal execution per canonical CallLeg (UNKNOWN included).
CREATE UNIQUE INDEX uq_telephony_legs_one_live_execution ON telephony_legs (calling_call_leg_id)
    WHERE status NOT IN ('ENDED', 'FAILED');
-- At most one non-terminal OUTBOUND execution per protected party of a Call (UNKNOWN
-- included): a failover attempt (a new CallLeg) can never run beside an unresolved or
-- live earlier attempt to the same destination (Part E §119, §146).
CREATE UNIQUE INDEX uq_telephony_legs_one_live_per_party ON telephony_legs (calling_call_id, contact_reference)
    WHERE direction = 'OUTBOUND' AND status NOT IN ('ENDED', 'FAILED');
CREATE INDEX idx_telephony_legs_call ON telephony_legs (calling_call_id);
CREATE INDEX idx_telephony_legs_status ON telephony_legs (status);
CREATE INDEX idx_telephony_legs_live_route ON telephony_legs (route_id)
    WHERE status NOT IN ('ENDED', 'FAILED');
CREATE INDEX idx_telephony_legs_live_provider ON telephony_legs (provider_id)
    WHERE status NOT IN ('ENDED', 'FAILED');
CREATE INDEX idx_telephony_legs_reconcile ON telephony_legs (reconcile_after)
    WHERE reconcile_after IS NOT NULL;
CREATE INDEX idx_telephony_legs_usage ON telephony_legs (usage_status, ended_at)
    WHERE usage_status = 'PENDING';
CREATE TRIGGER trg_telephony_legs_immutable
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'calling_call_id', 'calling_call_leg_id', 'calling_attempt_id', 'tenant_id',
        'application_id', 'environment_id', 'direction', 'route_class', 'generation',
        'contact_reference', 'relay_session_id', 'context_id', 'privacy_decision_id',
        'call_authorization_id', 'inbound_resolution_id', 'spend_reservation_id',
        'spend_reserved_minor', 'spend_currency', 'max_duration_seconds', 'media_attachment',
        'created_at');
CREATE TRIGGER trg_telephony_legs_set_once
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'route_id', 'provider_id', 'destination_country', 'provider_cost_minor', 'provider_cost_currency',
        'originating_at', 'ringing_at',
        'answered_at', 'connected_at', 'ended_at', 'end_reason', 'failure_category');
CREATE TRIGGER trg_telephony_legs_transition
    BEFORE UPDATE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_transition('status',
        'PLANNED>ORIGINATING', 'PLANNED>FAILED', 'PLANNED>ENDED',
        'ORIGINATING>RINGING', 'ORIGINATING>ANSWERED', 'ORIGINATING>CONNECTED', 'ORIGINATING>FAILED',
        'ORIGINATING>UNKNOWN', 'ORIGINATING>ENDING', 'ORIGINATING>ENDED',
        'RINGING>ANSWERED', 'RINGING>CONNECTED', 'RINGING>FAILED', 'RINGING>UNKNOWN',
        'RINGING>ENDING', 'RINGING>ENDED',
        'ANSWERED>CONNECTED', 'ANSWERED>HELD', 'ANSWERED>ENDING', 'ANSWERED>ENDED', 'ANSWERED>FAILED',
        'CONNECTED>HELD', 'CONNECTED>ENDING', 'CONNECTED>ENDED', 'CONNECTED>FAILED',
        'HELD>CONNECTED', 'HELD>ENDING', 'HELD>ENDED', 'HELD>FAILED',
        'ENDING>ENDED', 'ENDING>FAILED', 'ENDING>UNKNOWN',
        'UNKNOWN>RINGING', 'UNKNOWN>ANSWERED', 'UNKNOWN>CONNECTED', 'UNKNOWN>ENDING',
        'UNKNOWN>ENDED', 'UNKNOWN>FAILED');
CREATE TRIGGER trg_telephony_legs_no_delete
    BEFORE DELETE ON telephony_legs
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();

CREATE TABLE provider_call_bindings (
    id                       UUID PRIMARY KEY,
    telephony_leg_id         UUID        NOT NULL REFERENCES telephony_legs (id) ON DELETE RESTRICT,
    provider_id              VARCHAR(32) NOT NULL REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    route_id                 VARCHAR(64) NOT NULL REFERENCES telecom_routes (id) ON DELETE RESTRICT,
    correlation_key          VARCHAR(64) NOT NULL,
    provider_call_ref        VARCHAR(160),
    provider_idempotency_ref VARCHAR(160),
    status                   TEXT        NOT NULL,
    created_at               TIMESTAMPTZ NOT NULL,
    sent_at                  TIMESTAMPTZ,
    bound_at                 TIMESTAMPTZ,
    released_at              TIMESTAMPTZ,
    updated_at               TIMESTAMPTZ NOT NULL,
    CONSTRAINT uq_provider_call_bindings_correlation UNIQUE (correlation_key),
    CONSTRAINT uq_provider_call_bindings_provider_ref UNIQUE (provider_id, provider_call_ref),
    CONSTRAINT ck_provider_call_bindings_correlation CHECK (correlation_key ~ '^[a-z0-9-]{16,64}$'),
    CONSTRAINT ck_provider_call_bindings_ref
        CHECK (provider_call_ref IS NULL OR provider_call_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:@-]{0,159}$'),
    CONSTRAINT ck_provider_call_bindings_status
        CHECK (status IN ('PREPARED', 'SENT', 'BOUND', 'ABSENT', 'RELEASED')),
    CONSTRAINT ck_provider_call_bindings_bound
        CHECK ((status IN ('BOUND', 'RELEASED') AND provider_call_ref IS NOT NULL) OR status NOT IN ('BOUND')),
    CONSTRAINT ck_provider_call_bindings_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_provider_call_bindings_leg ON provider_call_bindings (telephony_leg_id);
CREATE TRIGGER trg_provider_call_bindings_immutable
    BEFORE UPDATE ON provider_call_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change(
        'id', 'telephony_leg_id', 'provider_id', 'route_id', 'correlation_key', 'created_at');
CREATE TRIGGER trg_provider_call_bindings_set_once
    BEFORE UPDATE ON provider_call_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_reassignment(
        'provider_call_ref', 'provider_idempotency_ref', 'sent_at', 'bound_at', 'released_at');
CREATE TRIGGER trg_provider_call_bindings_transition
    BEFORE UPDATE ON provider_call_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_guard_transition('status',
        'PREPARED>SENT', 'PREPARED>ABSENT', 'PREPARED>BOUND', 'SENT>BOUND', 'SENT>ABSENT',
        'SENT>RELEASED', 'BOUND>RELEASED');
CREATE TRIGGER trg_provider_call_bindings_no_delete
    BEFORE DELETE ON provider_call_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS provider_call_bindings;
DROP TABLE IF EXISTS telephony_legs;
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
