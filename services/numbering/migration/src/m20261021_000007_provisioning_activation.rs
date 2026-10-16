//! Workstream I: provisioning and activation prerequisites.
//!
//! * `number_acquisitions` — the durable provisioning intent of one reservation of a
//!   customer scope: the re-evaluated eligibility decision it rests on, the routable
//!   capabilities requested, the `PROVISION` command and, once the provider's acquisition is
//!   proven, the binding. One acquisition per reservation (a definitely failed one may be
//!   retried); an uncertain one leaves only through the command's reconciliation.
//! * `number_route_configurations` — provider-side voice / SMS route configuration per
//!   number and capability family: the opaque route reference, a gap-free route revision,
//!   the binding (and binding revision) it was configured on, the `CONFIGURE_*` command
//!   and the provider's acknowledgement evidence. One current configuration per family;
//!   a newer one supersedes it.
//! * `number_route_readiness` — the consumer that carries a family's traffic (Telephony
//!   for voice, the SMS Gateway for SMS) acknowledging a CONFIGURED route revision through
//!   its authenticated internal contract. Numbering never fabricates it.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261021_000007_provisioning_activation"
    }
}

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Acquisitions (provisioning intents of reservations)
-- ---------------------------------------------------------------------------
CREATE FUNCTION number_acquisition_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('PROVISIONING', 'PROVISIONED'), ('PROVISIONING', 'FAILED'), ('PROVISIONING', 'UNKNOWN'),
        ('UNKNOWN', 'PROVISIONED'), ('UNKNOWN', 'FAILED'),
        ('FAILED', 'PROVISIONING')
    )
$$;

CREATE TABLE number_acquisitions (
    id                       UUID         NOT NULL,
    reservation_id           UUID         NOT NULL,
    number_resource_id       UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    environment_kind         VARCHAR(16)  NOT NULL,
    status                   VARCHAR(16)  NOT NULL,
    requested_capabilities   TEXT[]       NOT NULL,
    eligibility_decision_ref UUID         NOT NULL,
    provision_command_id     UUID,
    binding_id               UUID,
    failure_code             VARCHAR(64),
    revision                 BIGINT       NOT NULL DEFAULT 1,
    correlation_id           VARCHAR(64)  NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    completed_at             TIMESTAMPTZ,
    CONSTRAINT pk_number_acquisitions PRIMARY KEY (id),
    CONSTRAINT uq_number_acquisitions_reservation UNIQUE (reservation_id),
    CONSTRAINT fk_number_acquisitions_reservation FOREIGN KEY (reservation_id, tenant_id, application_id, environment_id, number_resource_id)
        REFERENCES number_reservations (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT fk_number_acquisitions_command FOREIGN KEY (provision_command_id) REFERENCES number_provider_commands (id),
    CONSTRAINT fk_number_acquisitions_binding FOREIGN KEY (binding_id) REFERENCES number_provider_bindings (id),
    CONSTRAINT ck_number_acquisitions_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_number_acquisitions_status CHECK (status IN ('PROVISIONING', 'PROVISIONED', 'FAILED', 'UNKNOWN')),
    CONSTRAINT ck_number_acquisitions_capabilities CHECK (cardinality(requested_capabilities) >= 1
        AND requested_capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH']::TEXT[]),
    -- PROVISIONED only with the binding the provider's evidence created.
    CONSTRAINT ck_number_acquisitions_provisioned CHECK ((status = 'PROVISIONED') = (binding_id IS NOT NULL)),
    CONSTRAINT ck_number_acquisitions_failed CHECK ((status = 'FAILED') = (failure_code IS NOT NULL)),
    CONSTRAINT ck_number_acquisitions_completed CHECK ((status IN ('PROVISIONED', 'FAILED')) = (completed_at IS NOT NULL)),
    CONSTRAINT ck_number_acquisitions_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_acquisitions_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE INDEX ix_number_acquisitions_scope ON number_acquisitions (tenant_id, application_id, environment_id, created_at);
CREATE INDEX ix_number_acquisitions_resource ON number_acquisitions (number_resource_id);
CREATE INDEX ix_number_acquisitions_command ON number_acquisitions (provision_command_id);

CREATE FUNCTION number_guard_acquisition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'acquisitions are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        IF NEW.status <> 'PROVISIONING' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'an acquisition starts PROVISIONING at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status', 'provision_command_id', 'binding_id', 'failure_code', 'revision', 'updated_at',
            'completed_at', 'eligibility_decision_ref', 'requested_capabilities'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status', 'provision_command_id', 'binding_id', 'failure_code', 'revision', 'updated_at',
            'completed_at', 'eligibility_decision_ref', 'requested_capabilities']) THEN
        RAISE EXCEPTION 'the scope of an acquisition is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'acquisition updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status
       AND NOT number_acquisition_transition_allowed(OLD.status, NEW.status) THEN
        RAISE EXCEPTION 'acquisition transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
    END IF;
    -- A new attempt (FAILED -> PROVISIONING) is the only time the basis may change.
    IF (NEW.eligibility_decision_ref IS DISTINCT FROM OLD.eligibility_decision_ref
        OR NEW.requested_capabilities IS DISTINCT FROM OLD.requested_capabilities)
       AND NOT (OLD.status = 'FAILED' AND NEW.status = 'PROVISIONING') THEN
        RAISE EXCEPTION 'the basis of an acquisition attempt is immutable' USING ERRCODE = '42501';
    END IF;
    IF OLD.status = 'PROVISIONED' THEN
        RAISE EXCEPTION 'a PROVISIONED acquisition is final' USING ERRCODE = '42501';
    END IF;
    -- A provider outcome (proven or uncertain) always belongs to a recorded command.
    IF NEW.status IN ('PROVISIONED', 'UNKNOWN') AND NEW.provision_command_id IS NULL THEN
        RAISE EXCEPTION 'a % acquisition needs its PROVISION command', NEW.status USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_acquisitions_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_acquisitions
    FOR EACH ROW EXECUTE FUNCTION number_guard_acquisition();
CREATE TRIGGER trg_number_acquisitions_no_truncate
    BEFORE TRUNCATE ON number_acquisitions
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Provider-side route configuration (per number and capability family)
-- ---------------------------------------------------------------------------
CREATE TABLE number_route_configurations (
    id                 UUID         NOT NULL,
    number_resource_id UUID         NOT NULL,
    family             VARCHAR(8)   NOT NULL,
    route_ref          UUID         NOT NULL,
    route_revision     BIGINT       NOT NULL,
    binding_id         UUID         NOT NULL,
    binding_revision   BIGINT       NOT NULL,
    status             VARCHAR(16)  NOT NULL,
    command_id         UUID,
    evidence_ref       UUID,
    failure_code       VARCHAR(64),
    revision           BIGINT       NOT NULL DEFAULT 1,
    correlation_id     VARCHAR(64)  NOT NULL,
    requested_at       TIMESTAMPTZ  NOT NULL,
    acknowledged_at    TIMESTAMPTZ,
    superseded_at      TIMESTAMPTZ,
    updated_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_route_configurations PRIMARY KEY (id),
    CONSTRAINT uq_number_route_configurations_revision UNIQUE (number_resource_id, family, route_revision),
    CONSTRAINT fk_number_route_configurations_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_number_route_configurations_binding FOREIGN KEY (binding_id) REFERENCES number_provider_bindings (id),
    CONSTRAINT fk_number_route_configurations_command FOREIGN KEY (command_id) REFERENCES number_provider_commands (id),
    CONSTRAINT ck_number_route_configurations_family CHECK (family IN ('VOICE', 'SMS')),
    CONSTRAINT ck_number_route_configurations_status CHECK (status IN
        ('REQUESTED', 'CONFIGURED', 'FAILED', 'UNKNOWN', 'SUPERSEDED')),
    CONSTRAINT ck_number_route_configurations_revision CHECK (route_revision >= 1 AND binding_revision >= 1 AND revision >= 1),
    -- CONFIGURED only on the provider's acknowledgement evidence.
    CONSTRAINT ck_number_route_configurations_configured CHECK (status <> 'CONFIGURED'
        OR (evidence_ref IS NOT NULL AND acknowledged_at IS NOT NULL)),
    CONSTRAINT ck_number_route_configurations_failed CHECK ((status = 'FAILED') = (failure_code IS NOT NULL)),
    CONSTRAINT ck_number_route_configurations_superseded CHECK ((status = 'SUPERSEDED') = (superseded_at IS NOT NULL)),
    CONSTRAINT ck_number_route_configurations_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
-- One current configuration per number and family.
CREATE UNIQUE INDEX uq_number_route_configurations_current ON number_route_configurations (number_resource_id, family)
    WHERE status IN ('REQUESTED', 'CONFIGURED', 'UNKNOWN');
CREATE INDEX ix_number_route_configurations_command ON number_route_configurations (command_id);

CREATE FUNCTION number_route_configuration_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('REQUESTED', 'CONFIGURED'), ('REQUESTED', 'FAILED'), ('REQUESTED', 'UNKNOWN'), ('REQUESTED', 'SUPERSEDED'),
        ('UNKNOWN', 'CONFIGURED'), ('UNKNOWN', 'FAILED'), ('UNKNOWN', 'SUPERSEDED'),
        ('CONFIGURED', 'SUPERSEDED')
    )
$$;

CREATE FUNCTION number_guard_route_configuration() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'route configurations are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'REQUESTED' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'a route configuration starts REQUESTED at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status', 'command_id', 'evidence_ref', 'failure_code', 'revision', 'acknowledged_at',
            'superseded_at', 'updated_at'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status', 'command_id', 'evidence_ref', 'failure_code', 'revision', 'acknowledged_at',
            'superseded_at', 'updated_at']) THEN
        RAISE EXCEPTION 'the target of a route configuration is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'route configuration updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status
       AND NOT number_route_configuration_transition_allowed(OLD.status, NEW.status) THEN
        RAISE EXCEPTION 'route configuration transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
    END IF;
    IF OLD.status IN ('FAILED', 'SUPERSEDED') THEN
        RAISE EXCEPTION 'a % route configuration is history', OLD.status USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_route_configurations_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_route_configurations
    FOR EACH ROW EXECUTE FUNCTION number_guard_route_configuration();
CREATE TRIGGER trg_number_route_configurations_no_truncate
    BEFORE TRUNCATE ON number_route_configurations
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Consumer readiness of configured routes (authenticated acknowledgements)
-- ---------------------------------------------------------------------------
CREATE TABLE number_route_readiness (
    id                       UUID         NOT NULL,
    route_configuration_id   UUID         NOT NULL,
    number_resource_id       UUID         NOT NULL,
    family                   VARCHAR(8)   NOT NULL,
    route_revision           BIGINT       NOT NULL,
    consumer_service         VARCHAR(64)  NOT NULL,
    status                   VARCHAR(16)  NOT NULL,
    correlation_id           VARCHAR(64)  NOT NULL,
    acknowledged_at          TIMESTAMPTZ  NOT NULL,
    withdrawn_at             TIMESTAMPTZ,
    CONSTRAINT pk_number_route_readiness PRIMARY KEY (id),
    CONSTRAINT fk_number_route_readiness_configuration FOREIGN KEY (route_configuration_id) REFERENCES number_route_configurations (id),
    CONSTRAINT ck_number_route_readiness_family CHECK (family IN ('VOICE', 'SMS')),
    -- Voice traffic is carried by Telephony, SMS by the SMS Gateway: nobody else confirms.
    CONSTRAINT ck_number_route_readiness_consumer CHECK (
        (family = 'VOICE' AND consumer_service = 'cfmeet.telephony')
        OR (family = 'SMS' AND consumer_service = 'cfmeet.sms-gateway')),
    CONSTRAINT ck_number_route_readiness_status CHECK (status IN ('READY', 'WITHDRAWN')
        AND (status = 'WITHDRAWN') = (withdrawn_at IS NOT NULL)),
    CONSTRAINT ck_number_route_readiness_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE UNIQUE INDEX uq_number_route_readiness_current ON number_route_readiness (route_configuration_id, consumer_service)
    WHERE status = 'READY';

CREATE FUNCTION number_guard_route_readiness() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'route readiness is never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'READY' THEN
            RAISE EXCEPTION 'readiness starts READY' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status', 'withdrawn_at']) IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['status', 'withdrawn_at'])
       OR OLD.status = 'WITHDRAWN' THEN
        RAISE EXCEPTION 'readiness is only ever withdrawn' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_route_readiness_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_route_readiness
    FOR EACH ROW EXECUTE FUNCTION number_guard_route_readiness();
CREATE TRIGGER trg_number_route_readiness_no_truncate
    BEFORE TRUNCATE ON number_route_readiness
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_acquisitions) OR EXISTS (SELECT 1 FROM number_route_configurations) THEN
        RAISE EXCEPTION 'acquisitions or route configurations are recorded; they cannot be represented before Workstream I';
    END IF;
END $$;
DROP TABLE IF EXISTS number_route_readiness;
DROP FUNCTION IF EXISTS number_guard_route_readiness();
DROP TABLE IF EXISTS number_route_configurations;
DROP FUNCTION IF EXISTS number_guard_route_configuration();
DROP FUNCTION IF EXISTS number_route_configuration_transition_allowed(TEXT, TEXT);
DROP TABLE IF EXISTS number_acquisitions;
DROP FUNCTION IF EXISTS number_guard_acquisition();
DROP FUNCTION IF EXISTS number_acquisition_transition_allowed(TEXT, TEXT);
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
