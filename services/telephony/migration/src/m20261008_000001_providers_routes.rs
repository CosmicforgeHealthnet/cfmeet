//! Providers, routes, SIP peer configuration references and public-number bindings
//! (Part E §39–§43, §51).
//!
//! * `telecom_providers`: one row per configured interconnect (an adapter instance), with
//!   advertised capabilities, an operator kill switch, normalized health and a circuit
//!   breaker. No credential is ever stored: `credential_ref` names the deployment secret.
//! * `telecom_routes`: provider-neutral execution routes (countries, capabilities,
//!   priority, capacity, conservative cost estimate, duration cap, circuit, health).
//!   Their ids are the opaque route references Calling's Routing Engine receives.
//! * `sip_peer_config_refs`: references to SIP peer / trunk configuration held in the
//!   deployment's secret store (never a password, private key or peer secret).
//! * `telecom_number_bindings`: which interconnect routes may present / receive a
//!   platform-owned PUBLIC relay number (caller identity / inbound called number); one
//!   number may be bound to several routes (failover). Never a private number.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Columns that must never change after insert.
CREATE FUNCTION telephony_forbid_column_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col     text;
    old_row jsonb := to_jsonb(OLD);
    new_row jsonb := to_jsonb(NEW);
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (new_row -> col) IS DISTINCT FROM (old_row -> col) THEN
            RAISE EXCEPTION 'telephony invariant violated: %.% is immutable', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- Columns that may be set once (NULL -> value) and never changed afterwards.
CREATE FUNCTION telephony_forbid_reassignment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col     text;
    old_row jsonb := to_jsonb(OLD);
    new_row jsonb := to_jsonb(NEW);
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (old_row -> col) <> 'null'::jsonb AND (new_row -> col) IS DISTINCT FROM (old_row -> col) THEN
            RAISE EXCEPTION 'telephony invariant violated: %.% is set once', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- State graph guard: TG_ARGV[0] is the column, the rest are allowed 'FROM>TO' edges.
CREATE FUNCTION telephony_guard_transition() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col      text := TG_ARGV[0];
    old_val  text := to_jsonb(OLD) ->> TG_ARGV[0];
    new_val  text := to_jsonb(NEW) ->> TG_ARGV[0];
    i        integer;
BEGIN
    IF old_val IS NOT DISTINCT FROM new_val THEN
        RETURN NEW;
    END IF;
    FOR i IN 1 .. TG_NARGS - 1 LOOP
        IF TG_ARGV[i] = old_val || '>' || new_val THEN
            RETURN NEW;
        END IF;
    END LOOP;
    RAISE EXCEPTION 'telephony invariant violated: %.% % -> % is not allowed', TG_TABLE_NAME, col, old_val, new_val
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

CREATE FUNCTION telephony_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'telephony invariant violated: % rows are append-only', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

CREATE TABLE telecom_providers (
    id                   VARCHAR(32) PRIMARY KEY,
    adapter              TEXT        NOT NULL,
    capabilities         TEXT[]      NOT NULL,
    status               TEXT        NOT NULL,
    credential_ref       VARCHAR(128),
    circuit_state        TEXT        NOT NULL DEFAULT 'CLOSED',
    circuit_opened_at    TIMESTAMPTZ,
    consecutive_failures INTEGER     NOT NULL DEFAULT 0,
    health               TEXT        NOT NULL DEFAULT 'UNKNOWN',
    health_checked_at    TIMESTAMPTZ,
    max_concurrent_legs  INTEGER     NOT NULL,
    created_at           TIMESTAMPTZ NOT NULL,
    updated_at           TIMESTAMPTZ NOT NULL,
    version              BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT ck_telecom_providers_id CHECK (id ~ '^[a-z][a-z0-9-]{1,31}$'),
    CONSTRAINT ck_telecom_providers_adapter CHECK (adapter IN ('SIP_TRUNK', 'DETERMINISTIC_TEST')),
    CONSTRAINT ck_telecom_providers_capabilities CHECK (
        cardinality(capabilities) BETWEEN 1 AND 16 AND capabilities <@ ARRAY[
            'OUTBOUND_VOICE', 'INBOUND_VOICE', 'SIP_TRUNK', 'PUBLIC_NUMBER_BINDING', 'STATUS_QUERY',
            'EVENTS', 'HEALTH', 'CAPACITY', 'IDEMPOTENT_ORIGINATION', 'COST_ESTIMATE', 'DTMF',
            'CALLER_ID_SELECTION']::text[]),
    CONSTRAINT ck_telecom_providers_status CHECK (status IN ('ENABLED', 'DISABLED')),
    CONSTRAINT ck_telecom_providers_credential_ref
        CHECK (credential_ref IS NULL OR credential_ref ~ '^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$'),
    CONSTRAINT ck_telecom_providers_circuit CHECK (circuit_state IN ('CLOSED', 'OPEN', 'HALF_OPEN')),
    CONSTRAINT ck_telecom_providers_circuit_opened
        CHECK ((circuit_state = 'CLOSED') OR circuit_opened_at IS NOT NULL),
    CONSTRAINT ck_telecom_providers_health CHECK (health IN ('HEALTHY', 'DEGRADED', 'UNAVAILABLE', 'UNKNOWN')),
    CONSTRAINT ck_telecom_providers_failures CHECK (consecutive_failures >= 0),
    CONSTRAINT ck_telecom_providers_capacity CHECK (max_concurrent_legs BETWEEN 0 AND 100000),
    CONSTRAINT ck_telecom_providers_version CHECK (version >= 1),
    CONSTRAINT ck_telecom_providers_timestamps CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_telecom_providers_immutable
    BEFORE UPDATE ON telecom_providers
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change('id', 'adapter', 'created_at');

CREATE TABLE sip_peer_config_refs (
    id                     VARCHAR(64) PRIMARY KEY,
    provider_id            VARCHAR(32) NOT NULL REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    secret_ref             VARCHAR(128) NOT NULL,
    transport              TEXT        NOT NULL,
    allowed_source_cidrs   CIDR[]      NOT NULL,
    max_concurrent_dialogs INTEGER     NOT NULL,
    created_at             TIMESTAMPTZ NOT NULL,
    updated_at             TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_sip_peer_config_refs_id CHECK (id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_sip_peer_config_refs_secret_ref CHECK (secret_ref ~ '^[A-Za-z0-9][A-Za-z0-9._/-]{0,127}$'),
    CONSTRAINT ck_sip_peer_config_refs_transport CHECK (transport IN ('UDP', 'TCP', 'TLS')),
    CONSTRAINT ck_sip_peer_config_refs_cidrs CHECK (cardinality(allowed_source_cidrs) BETWEEN 1 AND 64),
    CONSTRAINT ck_sip_peer_config_refs_dialogs CHECK (max_concurrent_dialogs BETWEEN 1 AND 100000),
    CONSTRAINT ck_sip_peer_config_refs_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_sip_peer_config_refs_provider ON sip_peer_config_refs (provider_id);

CREATE TABLE telecom_routes (
    id                       VARCHAR(64) PRIMARY KEY,
    provider_id              VARCHAR(32) NOT NULL REFERENCES telecom_providers (id) ON DELETE RESTRICT,
    countries                TEXT[]      NOT NULL,
    capabilities             TEXT[]      NOT NULL,
    priority                 INTEGER     NOT NULL,
    status                   TEXT        NOT NULL,
    sip_peer_config_ref_id   VARCHAR(64) REFERENCES sip_peer_config_refs (id) ON DELETE RESTRICT,
    max_concurrent_legs      INTEGER     NOT NULL,
    estimated_leg_cost_minor BIGINT,
    cost_currency            CHAR(3),
    max_call_seconds         INTEGER     NOT NULL,
    circuit_state            TEXT        NOT NULL DEFAULT 'CLOSED',
    circuit_opened_at        TIMESTAMPTZ,
    consecutive_failures     INTEGER     NOT NULL DEFAULT 0,
    health                   TEXT        NOT NULL DEFAULT 'UNKNOWN',
    health_checked_at        TIMESTAMPTZ,
    created_at               TIMESTAMPTZ NOT NULL,
    updated_at               TIMESTAMPTZ NOT NULL,
    version                  BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT ck_telecom_routes_id CHECK (id ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$'),
    CONSTRAINT ck_telecom_routes_countries CHECK (
        cardinality(countries) BETWEEN 1 AND 250
        AND array_to_string(countries, ',') ~ '^[A-Z]{2}(,[A-Z]{2})*$'),
    CONSTRAINT ck_telecom_routes_capabilities CHECK (
        cardinality(capabilities) BETWEEN 1 AND 2
        AND capabilities <@ ARRAY['VOICE_OUTBOUND', 'VOICE_INBOUND']::text[]),
    CONSTRAINT ck_telecom_routes_priority CHECK (priority BETWEEN 0 AND 1000),
    CONSTRAINT ck_telecom_routes_status CHECK (status IN ('ENABLED', 'DISABLED')),
    CONSTRAINT ck_telecom_routes_capacity CHECK (max_concurrent_legs BETWEEN 0 AND 100000),
    CONSTRAINT ck_telecom_routes_cost CHECK (
        (estimated_leg_cost_minor IS NULL) = (cost_currency IS NULL)
        AND (estimated_leg_cost_minor IS NULL OR estimated_leg_cost_minor >= 0)
        AND (cost_currency IS NULL OR cost_currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_telecom_routes_duration CHECK (max_call_seconds BETWEEN 30 AND 86400),
    CONSTRAINT ck_telecom_routes_circuit CHECK (circuit_state IN ('CLOSED', 'OPEN', 'HALF_OPEN')),
    CONSTRAINT ck_telecom_routes_circuit_opened
        CHECK ((circuit_state = 'CLOSED') OR circuit_opened_at IS NOT NULL),
    CONSTRAINT ck_telecom_routes_health CHECK (health IN ('HEALTHY', 'DEGRADED', 'UNAVAILABLE', 'UNKNOWN')),
    CONSTRAINT ck_telecom_routes_failures CHECK (consecutive_failures >= 0),
    CONSTRAINT ck_telecom_routes_version CHECK (version >= 1),
    CONSTRAINT ck_telecom_routes_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_telecom_routes_selection ON telecom_routes USING gin (countries);
CREATE INDEX idx_telecom_routes_status_priority ON telecom_routes (status, priority, circuit_state);
CREATE INDEX idx_telecom_routes_provider ON telecom_routes (provider_id);
CREATE TRIGGER trg_telecom_routes_immutable
    BEFORE UPDATE ON telecom_routes
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change('id', 'provider_id', 'created_at');

CREATE TABLE telecom_number_bindings (
    public_e164  VARCHAR(16) NOT NULL,
    route_id     VARCHAR(64) NOT NULL REFERENCES telecom_routes (id) ON DELETE RESTRICT,
    capabilities TEXT[]      NOT NULL,
    status       TEXT        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_telecom_number_bindings PRIMARY KEY (public_e164, route_id),
    CONSTRAINT ck_telecom_number_bindings_e164 CHECK (public_e164 ~ '^\+[1-9][0-9]{6,14}$'),
    CONSTRAINT ck_telecom_number_bindings_capabilities CHECK (
        cardinality(capabilities) BETWEEN 1 AND 2
        AND capabilities <@ ARRAY['VOICE_OUTBOUND', 'VOICE_INBOUND']::text[]),
    CONSTRAINT ck_telecom_number_bindings_status CHECK (status IN ('ENABLED', 'DISABLED')),
    CONSTRAINT ck_telecom_number_bindings_timestamps CHECK (updated_at >= created_at)
);
CREATE INDEX idx_telecom_number_bindings_route ON telecom_number_bindings (route_id);
CREATE TRIGGER trg_telecom_number_bindings_immutable
    BEFORE UPDATE ON telecom_number_bindings
    FOR EACH ROW EXECUTE FUNCTION telephony_forbid_column_change('public_e164', 'route_id', 'created_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telecom_number_bindings;
DROP TABLE IF EXISTS telecom_routes;
DROP TABLE IF EXISTS sip_peer_config_refs;
DROP TABLE IF EXISTS telecom_providers;
DROP FUNCTION IF EXISTS telephony_forbid_mutation();
DROP FUNCTION IF EXISTS telephony_guard_transition();
DROP FUNCTION IF EXISTS telephony_forbid_reassignment();
DROP FUNCTION IF EXISTS telephony_forbid_column_change();
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
