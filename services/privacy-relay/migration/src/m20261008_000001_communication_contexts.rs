//! Communication Contexts (Part D §26–§29) and the shared Privacy Relay integrity
//! functions.
//!
//! A Communication Context is the generic, consumer-neutral authority object: WHY parties
//! may communicate (an opaque consumer reference), WHO (participants, migration 2), in
//! which DIRECTION, over which CHANNEL, during which time window, under which bounded
//! policy — and whether that authority is still valid. The consumer owns the business
//! object behind `external_context_reference`; Privacy Relay never parses it.
//!
//! Database-level guarantees:
//! * ownership columns are immutable; `(tenant, application, environment, id)` is the
//!   composite target every child row proves its ownership against;
//! * one context per (tenant, application, environment, external reference);
//! * the external reference is bounded and can never be a telephone number, e-mail
//!   address or SIP / tel URI;
//! * the policy snapshot is exactly the typed bounded `ContextPolicy` (four booleans),
//!   never an arbitrary document;
//! * `starts_at < expires_at <= grace_expires_at`;
//! * the lifecycle graph is enforced (REVOKED and EXPIRED are terminal and immutable,
//!   COMPLETED never returns to ACTIVE / SUSPENDED); every status change increments the
//!   version, and a version only ever moves forward by exactly one;
//! * contexts are never deleted.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Shared integrity functions (Privacy Relay owned).
-- ---------------------------------------------------------------------------
CREATE FUNCTION privacy_relay_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'privacy relay invariant violated: % rows are append-only (% refused)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

CREATE FUNCTION privacy_relay_forbid_column_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col     text;
    old_row jsonb := to_jsonb(OLD);
    new_row jsonb := to_jsonb(NEW);
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (new_row -> col) IS DISTINCT FROM (old_row -> col) THEN
            RAISE EXCEPTION 'privacy relay invariant violated: %.% is immutable', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- Columns that may be set once (NULL -> value) and never changed afterwards.
CREATE FUNCTION privacy_relay_forbid_reassignment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col     text;
    old_row jsonb := to_jsonb(OLD);
    new_row jsonb := to_jsonb(NEW);
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (old_row -> col) IS DISTINCT FROM 'null'::jsonb
           AND (new_row -> col) IS DISTINCT FROM (old_row -> col) THEN
            RAISE EXCEPTION 'privacy relay invariant violated: %.% cannot be reassigned', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- A bounded opaque consumer / platform reference that cannot carry contact data:
-- no telephone number (digits and separators only, >= 7 digits), no e-mail address,
-- no tel: / sip: / sips: / mailto: URI.
CREATE FUNCTION privacy_relay_safe_reference(value text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT value ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'
       AND NOT (value ~ '^[0-9.:-]+$' AND length(regexp_replace(value, '[^0-9]', '', 'g')) >= 7)
       AND lower(value) !~ '^(tel|sip|sips|mailto|callto|wa|sms):'
       AND position('@' in value) = 0
$$;

-- The typed bounded ContextPolicy: exactly these four boolean members.
CREATE FUNCTION privacy_relay_valid_context_policy(policy jsonb) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    expected text[] := ARRAY['grace_allows_new_calls',
                             'terminate_active_calls_on_participant_removal',
                             'terminate_active_calls_on_revoke',
                             'terminate_active_calls_on_suspend'];
    k text;
    n integer := 0;
BEGIN
    IF policy IS NULL OR jsonb_typeof(policy) <> 'object' THEN
        RETURN false;
    END IF;
    FOR k IN SELECT jsonb_object_keys(policy) LOOP
        IF NOT (k = ANY (expected)) OR jsonb_typeof(policy -> k) <> 'boolean' THEN
            RETURN false;
        END IF;
        n := n + 1;
    END LOOP;
    RETURN n = array_length(expected, 1);
END;
$$;

-- Relay strategies are policy vocabulary (not proof that a route exists).
CREATE FUNCTION privacy_relay_valid_strategies(strategies text[]) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT strategies IS NOT NULL
       AND cardinality(strategies) BETWEEN 1 AND 4
       AND strategies <@ ARRAY['APP_ONLY', 'APP_PREFERRED', 'PUBLIC_RELAY_ALLOWED', 'CALLBACK_RELAY_ALLOWED']::text[]
       AND cardinality(strategies) = (SELECT count(DISTINCT s) FROM unnest(strategies) AS s)
$$;

-- Lifecycle graph shared by contexts and relay sessions. Terminal rows never change.
CREATE FUNCTION privacy_relay_lifecycle_allowed(from_status text, to_status text) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT from_status NOT IN ('REVOKED', 'EXPIRED') AND (
        from_status = to_status
        OR (from_status, to_status) IN (
            ('ACTIVE', 'SUSPENDED'), ('ACTIVE', 'COMPLETED'), ('ACTIVE', 'REVOKED'), ('ACTIVE', 'EXPIRED'),
            ('SUSPENDED', 'ACTIVE'), ('SUSPENDED', 'COMPLETED'), ('SUSPENDED', 'REVOKED'), ('SUSPENDED', 'EXPIRED'),
            ('COMPLETED', 'REVOKED'), ('COMPLETED', 'EXPIRED')))
$$;

-- Versioned lifecycle guard for contexts and sessions: inserted ACTIVE at version 1; a
-- terminal row never changes; status follows the graph; the version moves forward by
-- exactly one (and must when the status changes); updated_at never runs backwards.
CREATE FUNCTION privacy_relay_guard_versioned_lifecycle() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.version <> 1 OR NEW.status <> 'ACTIVE' THEN
            RAISE EXCEPTION 'privacy relay invariant violated: % rows are created ACTIVE at version 1', TG_TABLE_NAME
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF NOT privacy_relay_lifecycle_allowed(OLD.status, NEW.status) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: %.status % -> % is not allowed', TG_TABLE_NAME, OLD.status, NEW.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NOT (NEW.version = OLD.version OR NEW.version = OLD.version + 1) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: %.version must advance by exactly one', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.status <> OLD.status AND NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a % status change must increment the version', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.updated_at < OLD.updated_at THEN
        RAISE EXCEPTION 'privacy relay invariant violated: %.updated_at must not move backwards', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- Communication Context.
-- ---------------------------------------------------------------------------
CREATE TABLE communication_contexts (
    id                            UUID PRIMARY KEY,
    tenant_id                     UUID         NOT NULL,
    application_id                UUID         NOT NULL,
    environment_id                UUID         NOT NULL,
    environment_kind              TEXT         NOT NULL,
    external_context_reference    VARCHAR(128) NOT NULL,
    version                       BIGINT       NOT NULL,
    status                        TEXT         NOT NULL,
    direction_policy              TEXT         NOT NULL,
    allowed_relay_strategies      TEXT[]       NOT NULL,
    starts_at                     TIMESTAMPTZ  NOT NULL,
    expires_at                    TIMESTAMPTZ  NOT NULL,
    grace_expires_at              TIMESTAMPTZ,
    -- End of ALL authority (worker index): the grace end when configured.
    final_expires_at              TIMESTAMPTZ  GENERATED ALWAYS AS (COALESCE(grace_expires_at, expires_at)) STORED,
    policy_version                VARCHAR(32)  NOT NULL,
    policy_snapshot               JSONB        NOT NULL,
    created_by_service_account_id UUID         NOT NULL,
    created_at                    TIMESTAMPTZ  NOT NULL,
    updated_at                    TIMESTAMPTZ  NOT NULL,
    suspended_at                  TIMESTAMPTZ,
    completed_at                  TIMESTAMPTZ,
    revoked_at                    TIMESTAMPTZ,
    expired_at                    TIMESTAMPTZ,
    CONSTRAINT uq_communication_contexts_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_communication_contexts_external_reference
        UNIQUE (tenant_id, application_id, environment_id, external_context_reference),
    CONSTRAINT ck_communication_contexts_environment_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_communication_contexts_reference CHECK (privacy_relay_safe_reference(external_context_reference)),
    CONSTRAINT ck_communication_contexts_version CHECK (version >= 1),
    CONSTRAINT ck_communication_contexts_status
        CHECK (status IN ('ACTIVE', 'SUSPENDED', 'REVOKED', 'COMPLETED', 'EXPIRED')),
    CONSTRAINT ck_communication_contexts_direction_policy CHECK (direction_policy IN ('BIDIRECTIONAL', 'EXPLICIT')),
    CONSTRAINT ck_communication_contexts_strategies CHECK (privacy_relay_valid_strategies(allowed_relay_strategies)),
    CONSTRAINT ck_communication_contexts_validity CHECK (starts_at < expires_at),
    CONSTRAINT ck_communication_contexts_grace CHECK (grace_expires_at IS NULL OR grace_expires_at >= expires_at),
    CONSTRAINT ck_communication_contexts_policy_version CHECK (policy_version ~ '^[A-Z][A-Z0-9_]{1,31}$'),
    CONSTRAINT ck_communication_contexts_policy CHECK (privacy_relay_valid_context_policy(policy_snapshot)),
    CONSTRAINT ck_communication_contexts_suspended CHECK (status <> 'SUSPENDED' OR suspended_at IS NOT NULL),
    CONSTRAINT ck_communication_contexts_completed CHECK (status <> 'COMPLETED' OR completed_at IS NOT NULL),
    CONSTRAINT ck_communication_contexts_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_communication_contexts_expired CHECK ((status = 'EXPIRED') = (expired_at IS NOT NULL)),
    CONSTRAINT ck_communication_contexts_updated CHECK (updated_at >= created_at)
);
CREATE INDEX idx_communication_contexts_scope_status
    ON communication_contexts (tenant_id, application_id, environment_id, status);
CREATE INDEX idx_communication_contexts_scope_created
    ON communication_contexts (tenant_id, application_id, environment_id, created_at DESC, id DESC);
-- Expiry worker: live contexts by end of all authority.
CREATE INDEX idx_communication_contexts_final_expiry
    ON communication_contexts (final_expires_at)
    WHERE status IN ('ACTIVE', 'SUSPENDED', 'COMPLETED');

CREATE TRIGGER trg_communication_contexts_lifecycle
    BEFORE INSERT OR UPDATE ON communication_contexts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_versioned_lifecycle();
CREATE TRIGGER trg_communication_contexts_immutable
    BEFORE UPDATE ON communication_contexts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'environment_kind',
        'external_context_reference', 'created_by_service_account_id', 'created_at');
CREATE TRIGGER trg_communication_contexts_set_once
    BEFORE UPDATE ON communication_contexts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('completed_at', 'revoked_at', 'expired_at');
CREATE TRIGGER trg_communication_contexts_no_delete
    BEFORE DELETE ON communication_contexts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS communication_contexts;
DROP FUNCTION IF EXISTS privacy_relay_guard_versioned_lifecycle();
DROP FUNCTION IF EXISTS privacy_relay_lifecycle_allowed(text, text);
DROP FUNCTION IF EXISTS privacy_relay_valid_strategies(text[]);
DROP FUNCTION IF EXISTS privacy_relay_valid_context_policy(jsonb);
DROP FUNCTION IF EXISTS privacy_relay_safe_reference(text);
DROP FUNCTION IF EXISTS privacy_relay_forbid_reassignment();
DROP FUNCTION IF EXISTS privacy_relay_forbid_column_change();
DROP FUNCTION IF EXISTS privacy_relay_forbid_mutation();
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
