//! Inbound relay resolution evidence (Part D §35).
//!
//! * `relay_inbound_resolutions`: one append-only row per inbound resolution attempt by
//!   Telephony — the outcome (RESOLVED, NO_ACTIVE_RELAY_MAPPING, AMBIGUOUS), a bounded
//!   reason, the inventory number / assignment / generation it concerned (when known),
//!   the scope / context / session and the versions it was resolved at, the presented
//!   generation and provider timestamps, and an opaque provider event reference. It
//!   never stores the called relay number, the caller's presented number or any
//!   destination: a RESOLVED row is what Telephony's inbound-caller protection is bound
//!   to (same service, short window).
//! * The inventory ledger gains RELAY_MAPPING_AMBIGUOUS (the fail-closed incident
//!   signal; the database makes the condition impossible, the code still checks).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE relay_inbound_resolutions (
    id                        UUID PRIMARY KEY,
    relay_number_id           UUID REFERENCES relay_number_inventory (id) ON DELETE RESTRICT,
    assignment_id             UUID REFERENCES relay_number_assignments (id) ON DELETE RESTRICT,
    assignment_generation     BIGINT,
    tenant_id                 UUID,
    application_id            UUID,
    environment_id            UUID,
    context_id                UUID,
    relay_session_id          UUID,
    context_version           BIGINT,
    session_version           BIGINT,
    outcome                   TEXT         NOT NULL,
    reason                    VARCHAR(48)  NOT NULL,
    presented_generation      BIGINT,
    provider_event_reference  VARCHAR(128),
    provider_observed_at      TIMESTAMPTZ  NOT NULL,
    provider_started_at       TIMESTAMPTZ,
    valid_until               TIMESTAMPTZ,
    requester_service         VARCHAR(64)  NOT NULL,
    request_id                VARCHAR(36)  NOT NULL,
    correlation_id            VARCHAR(37)  NOT NULL,
    decided_at                TIMESTAMPTZ  NOT NULL,
    CONSTRAINT fk_relay_inbound_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, relay_session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_relay_inbound_outcome CHECK (outcome IN ('RESOLVED', 'NO_ACTIVE_RELAY_MAPPING', 'AMBIGUOUS')),
    CONSTRAINT ck_relay_inbound_reason CHECK (reason ~ '^[A-Z][A-Z_]{1,47}$'),
    -- A resolution names its whole scope or none of it.
    CONSTRAINT ck_relay_inbound_scope CHECK (
        (tenant_id IS NULL) = (application_id IS NULL) AND (tenant_id IS NULL) = (environment_id IS NULL)
        AND (tenant_id IS NULL) = (context_id IS NULL) AND (tenant_id IS NULL) = (relay_session_id IS NULL)),
    CONSTRAINT ck_relay_inbound_resolved CHECK (
        outcome <> 'RESOLVED' OR (
            reason = 'RESOLVED' AND assignment_id IS NOT NULL AND tenant_id IS NOT NULL
            AND context_version IS NOT NULL AND session_version IS NOT NULL AND valid_until IS NOT NULL)),
    CONSTRAINT ck_relay_inbound_unresolved CHECK (outcome = 'RESOLVED' OR (reason <> 'RESOLVED' AND valid_until IS NULL)),
    CONSTRAINT ck_relay_inbound_generations CHECK (
        (assignment_generation IS NULL OR assignment_generation >= 1)
        AND (presented_generation IS NULL OR presented_generation >= 1)),
    CONSTRAINT ck_relay_inbound_reference CHECK (
        provider_event_reference IS NULL OR privacy_relay_safe_reference(provider_event_reference)),
    CONSTRAINT ck_relay_inbound_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
CREATE INDEX idx_relay_inbound_number ON relay_inbound_resolutions (relay_number_id, decided_at);
CREATE INDEX idx_relay_inbound_session ON relay_inbound_resolutions (relay_session_id, decided_at)
    WHERE relay_session_id IS NOT NULL;
CREATE TRIGGER trg_relay_inbound_append_only
    BEFORE UPDATE OR DELETE ON relay_inbound_resolutions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

ALTER TABLE relay_number_events DROP CONSTRAINT ck_relay_number_events_type;
ALTER TABLE relay_number_events ADD CONSTRAINT ck_relay_number_events_type CHECK (event_type IN (
    'RELAY_NUMBER_ADDED', 'RELAY_NUMBER_ACTIVATED', 'RELAY_NUMBER_ASSIGNED', 'RELAY_NUMBER_RELEASED',
    'RELAY_NUMBER_COOLDOWN_STARTED', 'RELAY_NUMBER_AVAILABLE', 'RELAY_NUMBER_DRAINING',
    'RELAY_NUMBER_DRAINED', 'RELAY_NUMBER_DISABLED', 'RELAY_MAPPING_AMBIGUOUS'));
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS relay_inbound_resolutions;
-- The ledger is immutable evidence: the narrower constraint is restored for new rows only.
ALTER TABLE relay_number_events DROP CONSTRAINT ck_relay_number_events_type;
ALTER TABLE relay_number_events ADD CONSTRAINT ck_relay_number_events_type CHECK (event_type IN (
    'RELAY_NUMBER_ADDED', 'RELAY_NUMBER_ACTIVATED', 'RELAY_NUMBER_ASSIGNED', 'RELAY_NUMBER_RELEASED',
    'RELAY_NUMBER_COOLDOWN_STARTED', 'RELAY_NUMBER_AVAILABLE', 'RELAY_NUMBER_DRAINING',
    'RELAY_NUMBER_DRAINED', 'RELAY_NUMBER_DISABLED')) NOT VALID;
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
