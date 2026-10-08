//! Public-network calls on the Calling side (Part E §44–§47, §153–§158).
//!
//! Calling keeps the logical Call, its canonical legs and the business evidence; Telephony
//! executes. Nothing here stores a telephone number, a provider / carrier identifier, a
//! SIP reference or a provider payload:
//!
//! * `calls.origin` — APPLICATION (created through the Calling API) or PUBLIC_INBOUND
//!   (offered by Telephony for a call to a relay number; §45).
//! * `call_public_parties` — the protected public parties of a Call by opaque
//!   ContactReference (and routing country), bound to the public CallLeg serving them.
//! * `call_telephony_commands` — Calling's durable outbox of commands to Telephony
//!   (ORIGINATE / TERMINATE / HOLD / RESUME / BRIDGE / UNBRIDGE). Persisted before any
//!   request; the payload never holds the Contact Resolution Grant secret (a fresh grant
//!   is obtained for every submission). One ORIGINATE and one TERMINATE per leg.
//! * `call_telephony_events` — dedupe ledger of the normalized Telephony events applied.
//! * `call_public_bridges` — Calling's view of a Telephony bridge (opaque id, status and,
//!   for APPLICATION_MEDIA, the reserved media identity admitted into the RTC resource).
//! * `call_spend_policies` / `call_spend_reservations` — durable telecom spend
//!   accounting: a reservation per paid leg, serialized on the environment's policy row
//!   (no check-then-spend race), settled from Telephony's usage fact.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE calls
    ADD COLUMN origin TEXT NOT NULL DEFAULT 'APPLICATION',
    ADD CONSTRAINT ck_calls_origin CHECK (origin IN ('APPLICATION', 'PUBLIC_INBOUND'));
CREATE TRIGGER trg_calls_origin_immutable
    BEFORE UPDATE ON calls
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('origin');

-- ---------------------------------------------------------------------------
-- Protected public parties (ContactReference only).
-- ---------------------------------------------------------------------------
CREATE TABLE call_public_parties (
    id                UUID PRIMARY KEY,
    call_id           UUID        NOT NULL,
    tenant_id         UUID        NOT NULL,
    application_id    UUID        NOT NULL,
    environment_id    UUID        NOT NULL,
    role              TEXT        NOT NULL,
    contact_reference UUID        NOT NULL,
    country           CHAR(2),
    call_leg_id       UUID,
    created_at        TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_public_parties_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_public_parties_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, call_leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_public_parties_contact UNIQUE (call_id, contact_reference),
    CONSTRAINT uq_call_public_parties_leg UNIQUE (call_leg_id),
    CONSTRAINT ck_call_public_parties_role CHECK (role IN ('CALLER', 'CALLEE')),
    CONSTRAINT ck_call_public_parties_country CHECK (country IS NULL OR country ~ '^[A-Z]{2}$')
);
CREATE INDEX idx_call_public_parties_call ON call_public_parties (call_id);
CREATE TRIGGER trg_call_public_parties_immutable
    BEFORE UPDATE ON call_public_parties
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'tenant_id', 'application_id', 'environment_id', 'role',
        'contact_reference', 'country', 'created_at');
-- A party is served by one public leg at a time. It moves to a new leg only after its
-- current leg FAILED (a failover attempt after a DEFINITE failure, Part E §51); never
-- while that leg is live or its outcome unknown, and never back to no leg.
CREATE FUNCTION call_public_parties_guard_leg() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.call_leg_id IS NOT NULL AND NEW.call_leg_id IS DISTINCT FROM OLD.call_leg_id THEN
        IF NEW.call_leg_id IS NULL OR NOT EXISTS (
            SELECT 1 FROM call_legs WHERE id = OLD.call_leg_id AND state = 'FAILED') THEN
            RAISE EXCEPTION 'calling invariant violated: a public party moves to a new leg only after its leg failed'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_public_parties_leg_guard
    BEFORE UPDATE ON call_public_parties
    FOR EACH ROW EXECUTE FUNCTION call_public_parties_guard_leg();
CREATE TRIGGER trg_call_public_parties_no_delete
    BEFORE DELETE ON call_public_parties
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Durable outbox of commands to Telephony.
-- ---------------------------------------------------------------------------
CREATE TABLE call_telephony_commands (
    id               UUID PRIMARY KEY,
    call_id          UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    call_leg_id      UUID,
    attempt_id       UUID,
    bridge_id        UUID,
    kind             TEXT         NOT NULL,
    idempotency_key  VARCHAR(128) NOT NULL,
    payload          JSONB        NOT NULL,
    status           TEXT         NOT NULL,
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_until      TIMESTAMPTZ,
    last_error_code  VARCHAR(64),
    telephony_ref    UUID,
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT fk_call_telephony_commands_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_telephony_commands_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, call_leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_telephony_commands_key UNIQUE (idempotency_key),
    CONSTRAINT ck_call_telephony_commands_kind
        CHECK (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE')),
    CONSTRAINT ck_call_telephony_commands_status
        CHECK (status IN ('PENDING', 'ACCEPTED', 'REFUSED', 'DEAD')),
    CONSTRAINT ck_call_telephony_commands_target CHECK (
        (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME') AND call_leg_id IS NOT NULL)
        OR (kind IN ('BRIDGE', 'UNBRIDGE') AND bridge_id IS NOT NULL)),
    CONSTRAINT ck_call_telephony_commands_key CHECK (idempotency_key ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_call_telephony_commands_payload
        CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 16384
               AND NOT (payload ? 'grantSecret') AND NOT (payload -> 'destination' ? 'grantSecret')),
    CONSTRAINT ck_call_telephony_commands_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_call_telephony_commands_completed
        CHECK ((status IN ('ACCEPTED', 'REFUSED', 'DEAD')) = (completed_at IS NOT NULL)),
    CONSTRAINT ck_call_telephony_commands_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_call_telephony_commands_leg_once ON call_telephony_commands (call_leg_id, kind)
    WHERE kind IN ('ORIGINATE', 'TERMINATE');
CREATE INDEX idx_call_telephony_commands_due ON call_telephony_commands (next_attempt_at)
    WHERE status = 'PENDING';
CREATE INDEX idx_call_telephony_commands_call ON call_telephony_commands (call_id);
CREATE TRIGGER trg_call_telephony_commands_immutable
    BEFORE UPDATE ON call_telephony_commands
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'tenant_id', 'application_id', 'environment_id', 'call_leg_id',
        'attempt_id', 'bridge_id', 'kind', 'idempotency_key', 'payload', 'created_at');
CREATE TRIGGER trg_call_telephony_commands_set_once
    BEFORE UPDATE ON call_telephony_commands
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('telephony_ref', 'completed_at');
CREATE TRIGGER trg_call_telephony_commands_status
    BEFORE UPDATE ON call_telephony_commands
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'PENDING>ACCEPTED', 'PENDING>REFUSED', 'PENDING>DEAD');
CREATE TRIGGER trg_call_telephony_commands_no_delete
    BEFORE DELETE ON call_telephony_commands
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Applied Telephony events (dedupe by Telephony event id).
-- ---------------------------------------------------------------------------
CREATE TABLE call_telephony_events (
    event_id         UUID PRIMARY KEY,
    call_id          UUID        NOT NULL,
    call_leg_id      UUID,
    telephony_leg_id UUID,
    bridge_id        UUID,
    event_type       VARCHAR(48) NOT NULL,
    leg_sequence     BIGINT,
    disposition      TEXT        NOT NULL,
    occurred_at      TIMESTAMPTZ NOT NULL,
    received_at      TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_call_telephony_events_type CHECK (event_type ~ '^[A-Z][A-Z_]{1,47}$'),
    CONSTRAINT ck_call_telephony_events_disposition
        CHECK (disposition IN ('APPLIED', 'STALE', 'IGNORED_TERMINAL', 'UNMATCHED'))
);
CREATE INDEX idx_call_telephony_events_call ON call_telephony_events (call_id, received_at);
CREATE TRIGGER trg_call_telephony_events_append_only
    BEFORE UPDATE OR DELETE ON call_telephony_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Calling's view of Telephony bridges.
-- ---------------------------------------------------------------------------
CREATE TABLE call_public_bridges (
    id                  UUID PRIMARY KEY,
    call_id             UUID        NOT NULL,
    tenant_id           UUID        NOT NULL,
    application_id      UUID        NOT NULL,
    environment_id      UUID        NOT NULL,
    kind                TEXT        NOT NULL,
    status              TEXT        NOT NULL,
    telephony_bridge_id UUID,
    media_identity      VARCHAR(64),
    created_at          TIMESTAMPTZ NOT NULL,
    established_at      TIMESTAMPTZ,
    ended_at            TIMESTAMPTZ,
    updated_at          TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_public_bridges_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_public_bridges_telephony UNIQUE (telephony_bridge_id),
    CONSTRAINT uq_call_public_bridges_identity UNIQUE (media_identity),
    CONSTRAINT ck_call_public_bridges_kind CHECK (kind IN ('APPLICATION_MEDIA', 'PUBLIC_LEGS')),
    CONSTRAINT ck_call_public_bridges_status
        CHECK (status IN ('REQUESTED', 'ATTACHING', 'ACTIVE', 'ENDED', 'FAILED')),
    CONSTRAINT ck_call_public_bridges_identity CHECK (
        (kind = 'APPLICATION_MEDIA') = (media_identity IS NOT NULL)
        AND (media_identity IS NULL OR media_identity ~ '^cfm-pstn-[0-9a-f]{32}$')),
    CONSTRAINT ck_call_public_bridges_ended CHECK ((status IN ('ENDED', 'FAILED')) = (ended_at IS NOT NULL)),
    CONSTRAINT ck_call_public_bridges_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_call_public_bridges_live ON call_public_bridges (call_id)
    WHERE status IN ('REQUESTED', 'ATTACHING', 'ACTIVE');
CREATE TRIGGER trg_call_public_bridges_immutable
    BEFORE UPDATE ON call_public_bridges
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'call_id', 'tenant_id', 'application_id', 'environment_id', 'kind',
        'media_identity', 'created_at');
CREATE TRIGGER trg_call_public_bridges_set_once
    BEFORE UPDATE ON call_public_bridges
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'telephony_bridge_id', 'established_at', 'ended_at');
CREATE TRIGGER trg_call_public_bridges_status
    BEFORE UPDATE ON call_public_bridges
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'REQUESTED>ATTACHING', 'REQUESTED>ACTIVE', 'REQUESTED>ENDED', 'REQUESTED>FAILED',
        'ATTACHING>ACTIVE', 'ATTACHING>ENDED', 'ATTACHING>FAILED',
        'ACTIVE>ENDED', 'ACTIVE>FAILED');
CREATE TRIGGER trg_call_public_bridges_no_delete
    BEFORE DELETE ON call_public_bridges
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Telecom spend: policy per environment (the serialization point) and reservations.
-- ---------------------------------------------------------------------------
CREATE TABLE call_spend_policies (
    tenant_id                    UUID        NOT NULL,
    application_id               UUID        NOT NULL,
    environment_id               UUID        NOT NULL,
    currency                     CHAR(3)     NOT NULL,
    leg_allowance_minor          BIGINT      NOT NULL,
    max_leg_seconds              INTEGER     NOT NULL,
    daily_limit_minor            BIGINT      NOT NULL,
    monthly_limit_minor          BIGINT      NOT NULL,
    max_concurrent_legs          INTEGER     NOT NULL,
    max_originations_per_minute  INTEGER     NOT NULL,
    max_per_party_per_hour     INTEGER     NOT NULL,
    -- Optional velocity per requesting principal / service account / communication
    -- context (NULL: not enforced for that dimension).
    max_per_principal_per_hour       INTEGER,
    max_per_service_account_per_hour INTEGER,
    max_per_context_per_hour         INTEGER,
    enabled                      BOOLEAN     NOT NULL,
    revision                     INTEGER     NOT NULL,
    created_at                   TIMESTAMPTZ NOT NULL,
    updated_at                   TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_call_spend_policies PRIMARY KEY (environment_id),
    CONSTRAINT fk_call_spend_policies_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_call_spend_policies_currency CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_call_spend_policies_amounts CHECK (
        leg_allowance_minor > 0 AND daily_limit_minor >= leg_allowance_minor
        AND monthly_limit_minor >= daily_limit_minor),
    CONSTRAINT ck_call_spend_policies_seconds CHECK (max_leg_seconds BETWEEN 30 AND 86400),
    CONSTRAINT ck_call_spend_policies_subject_rates CHECK (
        (max_per_principal_per_hour IS NULL OR max_per_principal_per_hour >= 1)
        AND (max_per_service_account_per_hour IS NULL OR max_per_service_account_per_hour >= 1)
        AND (max_per_context_per_hour IS NULL OR max_per_context_per_hour >= 1)),
    CONSTRAINT ck_call_spend_policies_rates CHECK (
        max_concurrent_legs BETWEEN 1 AND 100000 AND max_originations_per_minute BETWEEN 1 AND 100000
        AND max_per_party_per_hour BETWEEN 1 AND 1000),
    CONSTRAINT ck_call_spend_policies_revision CHECK (revision >= 1),
    CONSTRAINT ck_call_spend_policies_timestamps CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_call_spend_policies_immutable
    BEFORE UPDATE ON call_spend_policies
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'tenant_id', 'application_id', 'environment_id', 'created_at');

CREATE TABLE call_spend_reservations (
    id                   UUID PRIMARY KEY,
    tenant_id            UUID        NOT NULL,
    application_id       UUID        NOT NULL,
    environment_id       UUID        NOT NULL,
    call_id              UUID        NOT NULL,
    call_leg_id          UUID        NOT NULL,
    contact_reference    UUID,
    -- Who the paid leg is spent for (velocity and Part B credential caps).
    principal_id         UUID        NOT NULL,
    service_account_id   UUID        NOT NULL,
    credential_id        UUID        NOT NULL,
    context_ref          UUID,
    amount_minor         BIGINT      NOT NULL,
    currency             CHAR(3)     NOT NULL,
    max_duration_seconds INTEGER     NOT NULL,
    status               TEXT        NOT NULL,
    settled_minor        BIGINT,
    cost_known           BOOLEAN,
    billable_seconds     INTEGER,
    created_at           TIMESTAMPTZ NOT NULL,
    settled_at           TIMESTAMPTZ,
    CONSTRAINT fk_call_spend_reservations_leg
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, call_leg_id)
        REFERENCES call_legs (tenant_id, application_id, environment_id, call_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_spend_reservations_leg UNIQUE (call_leg_id),
    CONSTRAINT ck_call_spend_reservations_amount CHECK (amount_minor > 0),
    CONSTRAINT ck_call_spend_reservations_currency CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_call_spend_reservations_duration CHECK (max_duration_seconds BETWEEN 30 AND 86400),
    CONSTRAINT ck_call_spend_reservations_status CHECK (status IN ('RESERVED', 'SETTLED', 'RELEASED')),
    CONSTRAINT ck_call_spend_reservations_settled CHECK (
        (status = 'RESERVED') = (settled_at IS NULL)
        AND (status = 'SETTLED') = (settled_minor IS NOT NULL)
        AND (settled_minor IS NULL OR settled_minor >= 0)
        AND (billable_seconds IS NULL OR billable_seconds >= 0)),
    CONSTRAINT ck_call_spend_reservations_timestamps CHECK (settled_at IS NULL OR settled_at >= created_at)
);
CREATE INDEX idx_call_spend_reservations_env ON call_spend_reservations (environment_id, created_at);
CREATE INDEX idx_call_spend_reservations_open ON call_spend_reservations (environment_id)
    WHERE status = 'RESERVED';
CREATE INDEX idx_call_spend_reservations_contact ON call_spend_reservations (contact_reference, created_at);
CREATE INDEX idx_call_spend_reservations_application ON call_spend_reservations (application_id, created_at);
CREATE INDEX idx_call_spend_reservations_credential ON call_spend_reservations (credential_id, created_at);
CREATE INDEX idx_call_spend_reservations_principal ON call_spend_reservations (principal_id, created_at);
CREATE INDEX idx_call_spend_reservations_service_account ON call_spend_reservations (service_account_id, created_at);
CREATE INDEX idx_call_spend_reservations_context ON call_spend_reservations (context_ref, created_at)
    WHERE context_ref IS NOT NULL;
CREATE TRIGGER trg_call_spend_reservations_immutable
    BEFORE UPDATE ON call_spend_reservations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'call_id', 'call_leg_id',
        'contact_reference', 'principal_id', 'service_account_id', 'credential_id', 'context_ref',
        'amount_minor', 'currency', 'max_duration_seconds', 'created_at');
CREATE TRIGGER trg_call_spend_reservations_set_once
    BEFORE UPDATE ON call_spend_reservations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'settled_minor', 'cost_known', 'billable_seconds', 'settled_at');
CREATE TRIGGER trg_call_spend_reservations_status
    BEFORE UPDATE ON call_spend_reservations
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'RESERVED>SETTLED', 'RESERVED>RELEASED');
CREATE TRIGGER trg_call_spend_reservations_no_delete
    BEFORE DELETE ON call_spend_reservations
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_spend_reservations;
DROP TABLE IF EXISTS call_spend_policies;
DROP TABLE IF EXISTS call_public_bridges;
DROP TABLE IF EXISTS call_telephony_events;
DROP TABLE IF EXISTS call_telephony_commands;
DROP TABLE IF EXISTS call_public_parties;
DROP FUNCTION IF EXISTS call_public_parties_guard_leg();
DROP TRIGGER IF EXISTS trg_calls_origin_immutable ON calls;
ALTER TABLE calls
    DROP CONSTRAINT IF EXISTS ck_calls_origin,
    DROP COLUMN IF EXISTS origin;
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
