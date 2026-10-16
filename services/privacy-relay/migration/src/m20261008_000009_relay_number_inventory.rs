//! Temporary relay-number inventory (Part D §34).
//!
//! * `relay_number_inventory`: platform-owned PUBLIC relay identities (never a private
//!   user number) — canonical E.164, country / region, provisioned capabilities (voice
//!   only), an opaque internal provider binding (never exposed), the operational status
//!   (RESERVED, AVAILABLE, ASSIGNED, DRAINING, COOLDOWN, UNAVAILABLE; server-controlled
//!   transitions), a monotonic assignment generation and the drain / cooldown /
//!   unavailability stamps. This is temporary relay infrastructure, not a numbering
//!   product: no ownership, porting or customer numbers.
//! * `relay_number_events`: the append-only operational ledger of every inventory
//!   transition (added, activated, assigned, released, cooldown, available, draining,
//!   drained, disabled) with the generation and assignment it concerns. No tenant data,
//!   no number.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE FUNCTION privacy_relay_number_transition_allowed(from_status text, to_status text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT from_status = to_status OR (from_status, to_status) IN (
        ('RESERVED', 'AVAILABLE'), ('RESERVED', 'DRAINING'), ('RESERVED', 'UNAVAILABLE'),
        ('AVAILABLE', 'ASSIGNED'), ('AVAILABLE', 'DRAINING'), ('AVAILABLE', 'UNAVAILABLE'),
        ('ASSIGNED', 'COOLDOWN'), ('ASSIGNED', 'DRAINING'), ('ASSIGNED', 'UNAVAILABLE'),
        ('COOLDOWN', 'AVAILABLE'), ('COOLDOWN', 'DRAINING'), ('COOLDOWN', 'UNAVAILABLE'),
        ('DRAINING', 'UNAVAILABLE'),
        ('UNAVAILABLE', 'RESERVED'))
$$;

CREATE TABLE relay_number_inventory (
    id                     UUID PRIMARY KEY,
    canonical_public_e164  VARCHAR(16)  NOT NULL,
    country_iso2           CHAR(2)      NOT NULL,
    region                 VARCHAR(64),
    capabilities           TEXT[]       NOT NULL,
    provider_binding_ref   VARCHAR(128) NOT NULL,
    operational_status     TEXT         NOT NULL,
    generation             BIGINT       NOT NULL,
    created_at             TIMESTAMPTZ  NOT NULL,
    updated_at             TIMESTAMPTZ  NOT NULL,
    draining_at            TIMESTAMPTZ,
    cooldown_until         TIMESTAMPTZ,
    unavailable_at         TIMESTAMPTZ,
    CONSTRAINT uq_relay_number_e164 UNIQUE (canonical_public_e164),
    CONSTRAINT uq_relay_number_binding UNIQUE (provider_binding_ref),
    CONSTRAINT ck_relay_number_e164 CHECK (canonical_public_e164 ~ '^\+[1-9][0-9]{6,14}$'),
    CONSTRAINT ck_relay_number_country CHECK (country_iso2 ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_relay_number_region CHECK (region IS NULL OR region ~ '^[A-Z0-9][A-Z0-9_-]{0,63}$'),
    CONSTRAINT ck_relay_number_capabilities CHECK (
        cardinality(capabilities) BETWEEN 1 AND 2
        AND capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND']::text[]),
    -- Opaque and provider-neutral: never the number itself.
    CONSTRAINT ck_relay_number_binding CHECK (
        provider_binding_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'
        AND position(substr(canonical_public_e164, 2) IN provider_binding_ref) = 0),
    CONSTRAINT ck_relay_number_status CHECK (operational_status IN (
        'RESERVED', 'AVAILABLE', 'ASSIGNED', 'DRAINING', 'COOLDOWN', 'UNAVAILABLE')),
    CONSTRAINT ck_relay_number_generation CHECK (generation >= 0),
    CONSTRAINT ck_relay_number_cooldown CHECK (operational_status <> 'COOLDOWN' OR cooldown_until IS NOT NULL),
    CONSTRAINT ck_relay_number_draining CHECK (operational_status <> 'DRAINING' OR draining_at IS NOT NULL),
    CONSTRAINT ck_relay_number_unavailable CHECK ((operational_status = 'UNAVAILABLE') = (unavailable_at IS NOT NULL)),
    CONSTRAINT ck_relay_number_updated CHECK (updated_at >= created_at)
);
-- Allocation: country + status, least recently changed first; capabilities by GIN.
CREATE INDEX idx_relay_number_allocation
    ON relay_number_inventory (operational_status, country_iso2, updated_at, id);
CREATE INDEX idx_relay_number_capabilities ON relay_number_inventory USING GIN (capabilities);
CREATE INDEX idx_relay_number_cooldown ON relay_number_inventory (cooldown_until)
    WHERE operational_status IN ('COOLDOWN', 'DRAINING');

-- Server-controlled status graph; the generation only moves forward, by exactly one, on
-- AVAILABLE -> ASSIGNED.
CREATE FUNCTION privacy_relay_guard_relay_number() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.generation <> 0 OR NEW.operational_status NOT IN ('RESERVED', 'AVAILABLE') THEN
            RAISE EXCEPTION 'privacy relay invariant violated: relay numbers are added RESERVED or AVAILABLE at generation 0'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF NOT privacy_relay_number_transition_allowed(OLD.operational_status, NEW.operational_status) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: relay number % -> % is not allowed',
            OLD.operational_status, NEW.operational_status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF OLD.operational_status = 'AVAILABLE' AND NEW.operational_status = 'ASSIGNED' THEN
        IF NEW.generation <> OLD.generation + 1 THEN
            RAISE EXCEPTION 'privacy relay invariant violated: an assignment advances the relay number generation by one'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    ELSIF NEW.generation <> OLD.generation THEN
        RAISE EXCEPTION 'privacy relay invariant violated: the relay number generation only advances on assignment'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'privacy relay invariant violated: relay_number_inventory.updated_at must not move backwards'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_number_guard
    BEFORE INSERT OR UPDATE ON relay_number_inventory
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_relay_number();
CREATE TRIGGER trg_relay_number_immutable
    BEFORE UPDATE ON relay_number_inventory
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'canonical_public_e164', 'country_iso2', 'region', 'capabilities', 'provider_binding_ref', 'created_at');
CREATE TRIGGER trg_relay_number_no_delete
    BEFORE DELETE ON relay_number_inventory
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

CREATE TABLE relay_number_events (
    id               UUID PRIMARY KEY,
    relay_number_id  UUID        NOT NULL REFERENCES relay_number_inventory (id) ON DELETE RESTRICT,
    event_type       TEXT        NOT NULL,
    from_status      TEXT,
    to_status        TEXT        NOT NULL,
    generation       BIGINT      NOT NULL,
    assignment_id    UUID,
    reason           VARCHAR(32),
    actor_kind       TEXT        NOT NULL,
    actor_ref        VARCHAR(64) NOT NULL,
    occurred_at      TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_relay_number_events_type CHECK (event_type IN (
        'RELAY_NUMBER_ADDED', 'RELAY_NUMBER_ACTIVATED', 'RELAY_NUMBER_ASSIGNED', 'RELAY_NUMBER_RELEASED',
        'RELAY_NUMBER_COOLDOWN_STARTED', 'RELAY_NUMBER_AVAILABLE', 'RELAY_NUMBER_DRAINING',
        'RELAY_NUMBER_DRAINED', 'RELAY_NUMBER_DISABLED')),
    CONSTRAINT ck_relay_number_events_status CHECK (
        (from_status IS NULL OR from_status IN ('RESERVED', 'AVAILABLE', 'ASSIGNED', 'DRAINING', 'COOLDOWN', 'UNAVAILABLE'))
        AND to_status IN ('RESERVED', 'AVAILABLE', 'ASSIGNED', 'DRAINING', 'COOLDOWN', 'UNAVAILABLE')),
    CONSTRAINT ck_relay_number_events_reason CHECK (reason IS NULL OR reason ~ '^[A-Z][A-Z_]{0,31}$'),
    CONSTRAINT ck_relay_number_events_actor CHECK (
        actor_kind IN ('OPERATOR', 'SERVICE', 'SYSTEM') AND actor_ref ~ '^[a-z][a-z0-9._-]{0,63}$'),
    CONSTRAINT ck_relay_number_events_generation CHECK (generation >= 0)
);
CREATE INDEX idx_relay_number_events_number ON relay_number_events (relay_number_id, occurred_at);
CREATE TRIGGER trg_relay_number_events_append_only
    BEFORE UPDATE OR DELETE ON relay_number_events
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS relay_number_events;
DROP TABLE IF EXISTS relay_number_inventory;
DROP FUNCTION IF EXISTS privacy_relay_guard_relay_number();
DROP FUNCTION IF EXISTS privacy_relay_number_transition_allowed(text, text);
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
