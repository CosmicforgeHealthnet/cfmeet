//! Temporary relay-number assignments (Part D §34).
//!
//! * `relay_number_assignments`: one relay number lent to ONE Privacy Relay Session of
//!   one tenant / application / environment / context, at the number's generation. ACTIVE
//!   while it carries routing authority; when that ends (release, session revoke /
//!   completion / expiry, own expiry, number disabled) it moves — atomically with the
//!   inventory — to COOLDOWN, and after the cooldown to its terminal state (RELEASED,
//!   REVOKED or EXPIRED). Terminal rows never change again; a new assignment is a new row.
//! * Database guarantees: at most ONE assignment HOLDS a number (ACTIVE or COOLDOWN —
//!   so an active mapping is never ambiguous and a quarantined number cannot be re-lent);
//!   one relay identity per session (the session binding is set once and must name this
//!   session's assignment); an assignment never outlives its session's final authority
//!   and only shrinks; the inventory row must be ASSIGNED at exactly that generation; a
//!   number returns to AVAILABLE / RESERVED only when no assignment holds it.
//! * Relay audit events gain RELAY_NUMBER_ASSIGNED / _RELEASED / _COOLDOWN_STARTED (the
//!   tenant-visible part of the lifecycle; inventory-only transitions stay in
//!   `relay_number_events`).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE relay_number_assignments (
    id                     UUID PRIMARY KEY,
    relay_number_id        UUID         NOT NULL REFERENCES relay_number_inventory (id) ON DELETE RESTRICT,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    context_id             UUID         NOT NULL,
    relay_session_id       UUID         NOT NULL,
    assignment_generation  BIGINT       NOT NULL,
    country_iso2           CHAR(2)      NOT NULL,
    region                 VARCHAR(64),
    capabilities           TEXT[]       NOT NULL,
    status                 TEXT         NOT NULL,
    end_reason             VARCHAR(32),
    assigned_at            TIMESTAMPTZ  NOT NULL,
    expires_at             TIMESTAMPTZ  NOT NULL,
    released_at            TIMESTAMPTZ,
    cooldown_until         TIMESTAMPTZ,
    requested_by_service   VARCHAR(64)  NOT NULL,
    request_id             VARCHAR(36)  NOT NULL,
    correlation_id         VARCHAR(37)  NOT NULL,
    created_at             TIMESTAMPTZ  NOT NULL,
    updated_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT fk_relay_assignments_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, relay_session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    -- One relay identity per session, ever (the session binding is set once).
    CONSTRAINT uq_relay_assignments_session UNIQUE (relay_session_id),
    CONSTRAINT uq_relay_assignments_generation UNIQUE (relay_number_id, assignment_generation),
    CONSTRAINT ck_relay_assignments_generation CHECK (assignment_generation >= 1),
    CONSTRAINT ck_relay_assignments_country CHECK (country_iso2 ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_relay_assignments_capabilities CHECK (
        cardinality(capabilities) BETWEEN 1 AND 2
        AND capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND']::text[]),
    CONSTRAINT ck_relay_assignments_status CHECK (status IN ('ACTIVE', 'COOLDOWN', 'RELEASED', 'REVOKED', 'EXPIRED')),
    CONSTRAINT ck_relay_assignments_reason CHECK (end_reason IS NULL OR end_reason IN (
        'RELEASED', 'SESSION_REVOKED', 'SESSION_COMPLETED', 'SESSION_EXPIRED', 'ASSIGNMENT_EXPIRED', 'NUMBER_DISABLED')),
    CONSTRAINT ck_relay_assignments_validity CHECK (expires_at > assigned_at),
    -- ACTIVE <=> no end; every ended assignment carries its reason, release time and cooldown.
    CONSTRAINT ck_relay_assignments_end CHECK (
        (status = 'ACTIVE') = (released_at IS NULL)
        AND (released_at IS NULL) = (end_reason IS NULL)
        AND (released_at IS NULL) = (cooldown_until IS NULL)),
    CONSTRAINT ck_relay_assignments_cooldown CHECK (cooldown_until IS NULL OR cooldown_until > released_at),
    CONSTRAINT ck_relay_assignments_terminal CHECK (
        status NOT IN ('RELEASED', 'REVOKED', 'EXPIRED')
        OR (status = 'RELEASED' AND end_reason IN ('RELEASED', 'SESSION_COMPLETED'))
        OR (status = 'REVOKED' AND end_reason IN ('SESSION_REVOKED', 'NUMBER_DISABLED'))
        OR (status = 'EXPIRED' AND end_reason IN ('SESSION_EXPIRED', 'ASSIGNMENT_EXPIRED'))),
    CONSTRAINT ck_relay_assignments_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_relay_assignments_updated CHECK (updated_at >= created_at)
);
-- No ambiguous routing and no reuse during cooldown: one holder per number.
CREATE UNIQUE INDEX uq_relay_assignments_holder ON relay_number_assignments (relay_number_id)
    WHERE status IN ('ACTIVE', 'COOLDOWN');
CREATE INDEX idx_relay_assignments_expiry ON relay_number_assignments (expires_at) WHERE status = 'ACTIVE';
CREATE INDEX idx_relay_assignments_cooldown ON relay_number_assignments (cooldown_until) WHERE status = 'COOLDOWN';
CREATE INDEX idx_relay_assignments_status ON relay_number_assignments (status);
CREATE INDEX idx_relay_assignments_context ON relay_number_assignments (context_id);
CREATE INDEX idx_relay_assignments_number ON relay_number_assignments (relay_number_id, assignment_generation);

CREATE FUNCTION privacy_relay_guard_relay_assignment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    s privacy_relay_sessions%ROWTYPE;
    n relay_number_inventory%ROWTYPE;
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT * INTO s FROM privacy_relay_sessions WHERE id = NEW.relay_session_id;
        IF s.status <> 'ACTIVE' OR NEW.assigned_at < s.starts_at OR NEW.expires_at > s.final_expires_at
           OR s.relay_identity_binding_ref IS NOT NULL THEN
            RAISE EXCEPTION 'privacy relay invariant violated: a relay assignment needs an ACTIVE, unbound session and never outlives it'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        SELECT * INTO n FROM relay_number_inventory WHERE id = NEW.relay_number_id;
        IF n.operational_status <> 'ASSIGNED' OR n.generation <> NEW.assignment_generation
           OR n.country_iso2 <> NEW.country_iso2 OR NOT (n.capabilities @> NEW.capabilities)
           OR (NEW.region IS NOT NULL AND n.region IS DISTINCT FROM NEW.region) THEN
            RAISE EXCEPTION 'privacy relay invariant violated: a relay assignment needs its number ASSIGNED at the same generation'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        IF NEW.status <> 'ACTIVE' THEN
            RAISE EXCEPTION 'privacy relay invariant violated: relay assignments are created ACTIVE'
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.status IN ('RELEASED', 'REVOKED', 'EXPIRED') THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a terminal relay assignment never changes (a new assignment is a new row)'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NOT (NEW.status = OLD.status
            OR (OLD.status = 'ACTIVE' AND NEW.status = 'COOLDOWN')
            OR (OLD.status = 'COOLDOWN' AND NEW.status IN ('RELEASED', 'REVOKED', 'EXPIRED'))) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: relay assignment % -> % is not allowed', OLD.status, NEW.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.expires_at > OLD.expires_at THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a relay assignment validity only shrinks'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'privacy relay invariant violated: relay_number_assignments.updated_at must not move backwards'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_assignments_guard
    BEFORE INSERT OR UPDATE ON relay_number_assignments
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_relay_assignment();
CREATE TRIGGER trg_relay_assignments_immutable
    BEFORE UPDATE ON relay_number_assignments
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'relay_number_id', 'tenant_id', 'application_id', 'environment_id', 'context_id', 'relay_session_id',
        'assignment_generation', 'country_iso2', 'region', 'capabilities', 'assigned_at', 'requested_by_service',
        'request_id', 'correlation_id', 'created_at');
CREATE TRIGGER trg_relay_assignments_set_once
    BEFORE UPDATE ON relay_number_assignments
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('end_reason', 'released_at', 'cooldown_until');
CREATE TRIGGER trg_relay_assignments_no_delete
    BEFORE DELETE ON relay_number_assignments
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- A number is offered again (AVAILABLE / RESERVED) only when no assignment holds it.
CREATE FUNCTION privacy_relay_guard_relay_number_release() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.operational_status IN ('AVAILABLE', 'RESERVED')
       AND OLD.operational_status IS DISTINCT FROM NEW.operational_status
       AND EXISTS (SELECT 1 FROM relay_number_assignments
                   WHERE relay_number_id = NEW.id AND status IN ('ACTIVE', 'COOLDOWN')) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a relay number held by an assignment (active or cooling down) cannot be offered'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_number_release_guard
    BEFORE UPDATE ON relay_number_inventory
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_relay_number_release();

-- The session's relay identity binding names this session's own assignment.
CREATE FUNCTION privacy_relay_guard_session_binding() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.relay_identity_binding_ref IS NOT NULL
       AND NEW.relay_identity_binding_ref IS DISTINCT FROM OLD.relay_identity_binding_ref
       AND NOT EXISTS (SELECT 1 FROM relay_number_assignments
                       WHERE id = NEW.relay_identity_binding_ref AND relay_session_id = NEW.id) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: relay_identity_binding_ref must reference this session''s relay assignment'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_sessions_binding
    BEFORE UPDATE OF relay_identity_binding_ref ON privacy_relay_sessions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_session_binding();

ALTER TABLE relay_number_events
    ADD CONSTRAINT fk_relay_number_events_assignment FOREIGN KEY (assignment_id)
        REFERENCES relay_number_assignments (id) ON DELETE RESTRICT;

ALTER TABLE relay_audit_events DROP CONSTRAINT ck_relay_audit_event_type;
ALTER TABLE relay_audit_events ADD CONSTRAINT ck_relay_audit_event_type CHECK (event_type IN (
    'CONTEXT_CREATED', 'CONTEXT_UPDATED', 'CONTEXT_PARTICIPANTS_CHANGED', 'CONTEXT_SUSPENDED',
    'CONTEXT_RESUMED', 'CONTEXT_REVOKED', 'CONTEXT_COMPLETED', 'CONTEXT_EXPIRED',
    'RELAY_SESSION_CREATED', 'RELAY_SESSION_UPDATED', 'RELAY_SESSION_SUSPENDED', 'RELAY_SESSION_RESUMED',
    'RELAY_SESSION_REVOKED', 'RELAY_SESSION_COMPLETED', 'RELAY_SESSION_EXPIRED',
    'ENFORCEMENT_REQUESTED', 'ENFORCEMENT_APPLIED', 'ENFORCEMENT_FAILED',
    'RELAY_NUMBER_ASSIGNED', 'RELAY_NUMBER_RELEASED', 'RELAY_NUMBER_COOLDOWN_STARTED'));
"#;

const DOWN: &str = r#"
ALTER TABLE relay_number_events DROP CONSTRAINT IF EXISTS fk_relay_number_events_assignment;
DROP TRIGGER IF EXISTS trg_relay_sessions_binding ON privacy_relay_sessions;
DROP TRIGGER IF EXISTS trg_relay_number_release_guard ON relay_number_inventory;
DROP TABLE IF EXISTS relay_number_assignments;
DROP FUNCTION IF EXISTS privacy_relay_guard_session_binding();
DROP FUNCTION IF EXISTS privacy_relay_guard_relay_number_release();
DROP FUNCTION IF EXISTS privacy_relay_guard_relay_assignment();
-- Audit events are immutable evidence: the narrower constraint is restored for new rows only.
ALTER TABLE relay_audit_events DROP CONSTRAINT ck_relay_audit_event_type;
ALTER TABLE relay_audit_events ADD CONSTRAINT ck_relay_audit_event_type CHECK (event_type IN (
    'CONTEXT_CREATED', 'CONTEXT_UPDATED', 'CONTEXT_PARTICIPANTS_CHANGED', 'CONTEXT_SUSPENDED',
    'CONTEXT_RESUMED', 'CONTEXT_REVOKED', 'CONTEXT_COMPLETED', 'CONTEXT_EXPIRED',
    'RELAY_SESSION_CREATED', 'RELAY_SESSION_UPDATED', 'RELAY_SESSION_SUSPENDED', 'RELAY_SESSION_RESUMED',
    'RELAY_SESSION_REVOKED', 'RELAY_SESSION_COMPLETED', 'RELAY_SESSION_EXPIRED',
    'ENFORCEMENT_REQUESTED', 'ENFORCEMENT_APPLIED', 'ENFORCEMENT_FAILED')) NOT VALID;
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
