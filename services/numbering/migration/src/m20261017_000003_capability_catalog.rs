//! Workstream E: the versioned country / number-type catalog and capability evidence
//! lifecycle (expiry, revocation, source freshness, binding provenance, change kind).
//!
//! * `number_catalog_revisions` — append-only, gap-free catalog revisions (every catalog
//!   change publishes exactly one).
//! * `number_country_catalog` / `number_type_catalog` — append-oriented: a change closes
//!   the CURRENT row (SUPERSEDED at a revision) and inserts the next one. Real and
//!   synthetic inventory have separate catalogs (`inventory_class`); synthetic entries are
//!   never VERIFIED and never apply to real numbers. A VERIFIED entry needs evidence.
//! * `number_capabilities` gains expiry, revocation / expiry statuses, the source's own
//!   observation time (stale-update protection), the binding that confirmed it and the
//!   kind of change; `number_resources.capability_revision` versions per-resource
//!   capability truth for consumers.
//!
//! Down restores the 000002 definitions exactly (guards and status vocabulary), after
//! closing nothing: capability rows written with the new statuses must be removed by the
//! operator first (the down migration refuses otherwise rather than lose history).

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261017_000003_capability_catalog"
    }
}

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Catalog revisions
-- ---------------------------------------------------------------------------
CREATE TABLE number_catalog_revisions (
    revision       BIGINT       NOT NULL,
    actor_kind     VARCHAR(16)  NOT NULL,
    actor_ref      VARCHAR(200) NOT NULL,
    reason_code    VARCHAR(64)  NOT NULL,
    correlation_id VARCHAR(64)  NOT NULL,
    published_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_catalog_revisions PRIMARY KEY (revision),
    CONSTRAINT ck_number_catalog_revisions_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_catalog_revisions_actor CHECK (actor_kind IN
        ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER')),
    CONSTRAINT ck_number_catalog_revisions_actor_ref CHECK (actor_ref ~ '^[A-Za-z0-9._:@-]{1,200}$'),
    CONSTRAINT ck_number_catalog_revisions_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_catalog_revisions_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);

CREATE FUNCTION number_guard_catalog_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'catalog revisions are append-only' USING ERRCODE = '42501';
    END IF;
    -- Gap-free and serialized: writers take the catalog advisory lock first.
    PERFORM pg_advisory_xact_lock(hashtext('number_catalog'));
    IF NEW.revision <> (SELECT COALESCE(MAX(revision), 0) + 1 FROM number_catalog_revisions) THEN
        RAISE EXCEPTION 'catalog revisions are consecutive' USING ERRCODE = '40001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_catalog_revisions_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_catalog_revisions
    FOR EACH ROW EXECUTE FUNCTION number_guard_catalog_revision();
CREATE TRIGGER trg_number_catalog_revisions_no_truncate
    BEFORE TRUNCATE ON number_catalog_revisions
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Country catalog
-- ---------------------------------------------------------------------------
CREATE TABLE number_country_catalog (
    id                      UUID         NOT NULL,
    country_code            CHAR(2)      NOT NULL,
    inventory_class         VARCHAR(16)  NOT NULL,
    calling_code            VARCHAR(3)   NOT NULL,
    support_status          VARCHAR(16)  NOT NULL,
    restriction_flags       TEXT[]       NOT NULL DEFAULT '{}',
    jurisdiction_policy_ref UUID,
    source                  VARCHAR(32)  NOT NULL,
    verification_status     VARCHAR(16)  NOT NULL,
    evidence_ref            UUID,
    effective_from          TIMESTAMPTZ  NOT NULL,
    effective_to            TIMESTAMPTZ,
    catalog_revision        BIGINT       NOT NULL,
    status                  VARCHAR(16)  NOT NULL DEFAULT 'CURRENT',
    superseded_at_revision  BIGINT,
    recorded_by             VARCHAR(200) NOT NULL,
    recorded_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_country_catalog PRIMARY KEY (id),
    CONSTRAINT fk_number_country_catalog_revision FOREIGN KEY (catalog_revision) REFERENCES number_catalog_revisions (revision),
    CONSTRAINT fk_number_country_catalog_superseded FOREIGN KEY (superseded_at_revision) REFERENCES number_catalog_revisions (revision),
    CONSTRAINT ck_number_country_catalog_country CHECK (country_code ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_number_country_catalog_calling_code CHECK (calling_code ~ '^[1-9][0-9]{0,2}$'),
    CONSTRAINT ck_number_country_catalog_class CHECK (inventory_class IN ('REAL', 'SYNTHETIC')),
    CONSTRAINT ck_number_country_catalog_support CHECK (support_status IN ('SUPPORTED', 'RESTRICTED', 'UNSUPPORTED')),
    CONSTRAINT ck_number_country_catalog_restrictions CHECK (restriction_flags <@ ARRAY[
        'NO_INTERNATIONAL_OUTBOUND', 'NO_PREMIUM_DESTINATIONS', 'KYC_REQUIRED', 'LOCAL_PRESENCE_REQUIRED',
        'SMS_REGISTRATION_REQUIRED', 'EMERGENCY_SERVICES_UNSUPPORTED']::TEXT[]),
    CONSTRAINT ck_number_country_catalog_source CHECK (source IN
        ('REGULATOR_PUBLICATION', 'PROVIDER_DOCUMENTATION', 'OPERATOR_ATTESTATION', 'SYNTHETIC_TEST')),
    CONSTRAINT ck_number_country_catalog_verification CHECK (verification_status IN ('VERIFIED', 'UNVERIFIED', 'SYNTHETIC')),
    -- Synthetic test catalogs are exactly the synthetic ones; VERIFIED needs evidence.
    CONSTRAINT ck_number_country_catalog_synthetic CHECK (
        (inventory_class = 'SYNTHETIC') = (source = 'SYNTHETIC_TEST')
        AND (inventory_class = 'SYNTHETIC') = (verification_status = 'SYNTHETIC')),
    CONSTRAINT ck_number_country_catalog_verified CHECK (verification_status <> 'VERIFIED' OR evidence_ref IS NOT NULL),
    CONSTRAINT ck_number_country_catalog_period CHECK (effective_to IS NULL OR effective_to > effective_from),
    CONSTRAINT ck_number_country_catalog_status CHECK (status IN ('CURRENT', 'SUPERSEDED')
        AND (status = 'SUPERSEDED') = (superseded_at_revision IS NOT NULL)),
    CONSTRAINT ck_number_country_catalog_recorded_by CHECK (recorded_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
CREATE UNIQUE INDEX uq_number_country_catalog_current ON number_country_catalog (country_code, inventory_class)
    WHERE status = 'CURRENT';

-- ---------------------------------------------------------------------------
-- Number-type catalog (per country, inventory class, type, optional region / provider)
-- ---------------------------------------------------------------------------
CREATE TABLE number_type_catalog (
    id                       UUID         NOT NULL,
    country_code             CHAR(2)      NOT NULL,
    inventory_class          VARCHAR(16)  NOT NULL,
    number_type              VARCHAR(16)  NOT NULL,
    locality_class           VARCHAR(16)  NOT NULL,
    region                   VARCHAR(100),
    provider_id              VARCHAR(64),
    supported_capabilities   TEXT[]       NOT NULL DEFAULT '{}',
    unsupported_capabilities TEXT[]       NOT NULL DEFAULT '{}',
    limitations              JSONB        NOT NULL DEFAULT '{}'::jsonb,
    restriction_flags        TEXT[]       NOT NULL DEFAULT '{}',
    jurisdiction_policy_ref  UUID,
    source                   VARCHAR(32)  NOT NULL,
    verification_status      VARCHAR(16)  NOT NULL,
    evidence_ref             UUID,
    effective_from           TIMESTAMPTZ  NOT NULL,
    effective_to             TIMESTAMPTZ,
    catalog_revision         BIGINT       NOT NULL,
    status                   VARCHAR(16)  NOT NULL DEFAULT 'CURRENT',
    superseded_at_revision   BIGINT,
    recorded_by              VARCHAR(200) NOT NULL,
    recorded_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_type_catalog PRIMARY KEY (id),
    CONSTRAINT fk_number_type_catalog_revision FOREIGN KEY (catalog_revision) REFERENCES number_catalog_revisions (revision),
    CONSTRAINT fk_number_type_catalog_superseded FOREIGN KEY (superseded_at_revision) REFERENCES number_catalog_revisions (revision),
    CONSTRAINT ck_number_type_catalog_country CHECK (country_code ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_number_type_catalog_class CHECK (inventory_class IN ('REAL', 'SYNTHETIC')),
    CONSTRAINT ck_number_type_catalog_type CHECK (number_type IN ('GEOGRAPHIC', 'MOBILE', 'TOLL_FREE', 'NATIONAL')),
    CONSTRAINT ck_number_type_catalog_locality CHECK (locality_class IN ('NATIONAL', 'REGIONAL', 'LOCAL', 'NON_GEOGRAPHIC')),
    CONSTRAINT ck_number_type_catalog_region CHECK (region IS NULL OR length(btrim(region)) BETWEEN 1 AND 100),
    CONSTRAINT ck_number_type_catalog_provider CHECK (provider_id IS NULL OR provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_type_catalog_capabilities CHECK (
        supported_capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH',
            'MOBILE_NUMBER_CLASS', 'GEOGRAPHIC_LOCAL', 'TOLL_FREE_BUSINESS']::TEXT[]
        AND unsupported_capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH',
            'MOBILE_NUMBER_CLASS', 'GEOGRAPHIC_LOCAL', 'TOLL_FREE_BUSINESS']::TEXT[]
        AND NOT (supported_capabilities && unsupported_capabilities)),
    -- Number-class attributes follow the type (SMS support never makes a number mobile).
    CONSTRAINT ck_number_type_catalog_class_attributes CHECK (
        ('MOBILE_NUMBER_CLASS' <> ALL (supported_capabilities) OR number_type = 'MOBILE')
        AND ('GEOGRAPHIC_LOCAL' <> ALL (supported_capabilities) OR number_type = 'GEOGRAPHIC')
        AND ('TOLL_FREE_BUSINESS' <> ALL (supported_capabilities) OR number_type = 'TOLL_FREE')),
    CONSTRAINT ck_number_type_catalog_limitations CHECK (jsonb_typeof(limitations) = 'object'),
    CONSTRAINT ck_number_type_catalog_restrictions CHECK (restriction_flags <@ ARRAY[
        'NO_INTERNATIONAL_OUTBOUND', 'NO_PREMIUM_DESTINATIONS', 'KYC_REQUIRED', 'LOCAL_PRESENCE_REQUIRED',
        'SMS_REGISTRATION_REQUIRED', 'EMERGENCY_SERVICES_UNSUPPORTED']::TEXT[]),
    CONSTRAINT ck_number_type_catalog_source CHECK (source IN
        ('REGULATOR_PUBLICATION', 'PROVIDER_DOCUMENTATION', 'OPERATOR_ATTESTATION', 'SYNTHETIC_TEST')),
    CONSTRAINT ck_number_type_catalog_verification CHECK (verification_status IN ('VERIFIED', 'UNVERIFIED', 'SYNTHETIC')),
    CONSTRAINT ck_number_type_catalog_synthetic CHECK (
        (inventory_class = 'SYNTHETIC') = (source = 'SYNTHETIC_TEST')
        AND (inventory_class = 'SYNTHETIC') = (verification_status = 'SYNTHETIC')
        AND (inventory_class = 'REAL' OR provider_id IS NULL OR provider_id = 'synthetic')),
    CONSTRAINT ck_number_type_catalog_verified CHECK (verification_status <> 'VERIFIED' OR evidence_ref IS NOT NULL),
    CONSTRAINT ck_number_type_catalog_period CHECK (effective_to IS NULL OR effective_to > effective_from),
    CONSTRAINT ck_number_type_catalog_status CHECK (status IN ('CURRENT', 'SUPERSEDED')
        AND (status = 'SUPERSEDED') = (superseded_at_revision IS NOT NULL)),
    CONSTRAINT ck_number_type_catalog_recorded_by CHECK (recorded_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
CREATE UNIQUE INDEX uq_number_type_catalog_current ON number_type_catalog
    (country_code, inventory_class, number_type, COALESCE(region, ''), COALESCE(provider_id, ''))
    WHERE status = 'CURRENT';

-- Catalog rows only ever close; their content is history.
CREATE FUNCTION number_guard_catalog_entry() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'catalog history is never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'CURRENT' THEN
            RAISE EXCEPTION 'a catalog entry is inserted CURRENT' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.status <> 'CURRENT' OR NEW.status <> 'SUPERSEDED'
       OR (to_jsonb(NEW) - 'status' - 'superseded_at_revision') IS DISTINCT FROM
          (to_jsonb(OLD) - 'status' - 'superseded_at_revision') THEN
        RAISE EXCEPTION 'a catalog entry only ever becomes SUPERSEDED' USING ERRCODE = '42501';
    END IF;
    IF NEW.superseded_at_revision <= OLD.catalog_revision THEN
        RAISE EXCEPTION 'an entry is superseded by a later catalog revision' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_country_catalog_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_country_catalog
    FOR EACH ROW EXECUTE FUNCTION number_guard_catalog_entry();
CREATE TRIGGER trg_number_country_catalog_no_truncate
    BEFORE TRUNCATE ON number_country_catalog
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_type_catalog_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_type_catalog
    FOR EACH ROW EXECUTE FUNCTION number_guard_catalog_entry();
CREATE TRIGGER trg_number_type_catalog_no_truncate
    BEFORE TRUNCATE ON number_type_catalog
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Capability evidence lifecycle
-- ---------------------------------------------------------------------------
ALTER TABLE number_resources ADD COLUMN capability_revision BIGINT NOT NULL DEFAULT 0;
ALTER TABLE number_resources ADD CONSTRAINT ck_number_resources_capability_revision CHECK (capability_revision >= 0);

ALTER TABLE number_capabilities
    ADD COLUMN expires_at          TIMESTAMPTZ,
    ADD COLUMN source_observed_at  TIMESTAMPTZ,
    ADD COLUMN binding_id          UUID,
    ADD COLUMN capability_revision BIGINT,
    ADD COLUMN change_kind         VARCHAR(16),
    ADD COLUMN actor_kind          VARCHAR(16),
    ADD COLUMN actor_ref           VARCHAR(200),
    ADD COLUMN reason_code         VARCHAR(64);
ALTER TABLE number_capabilities
    ADD CONSTRAINT fk_number_capabilities_binding FOREIGN KEY (binding_id) REFERENCES number_provider_bindings (id);
ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_status;
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_status CHECK (status IN
    ('VERIFIED', 'DECLARED', 'RESTRICTED', 'WITHDRAWN', 'EXPIRED', 'REVOKED'));
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_expiry CHECK (
    expires_at IS NULL OR expires_at > effective_from);
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_change_kind CHECK (change_kind IS NULL OR change_kind IN
    ('INITIAL', 'UPGRADE', 'DOWNGRADE', 'REFRESH', 'REVOCATION', 'EXPIRY'));
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_actor CHECK (actor_kind IS NULL OR actor_kind IN
    ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER'));
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_reason CHECK (
    reason_code IS NULL OR reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$');
-- Every record written from Workstream E on carries its revision and change provenance;
-- a provider confirmation names the binding that confirmed it.
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_provenance CHECK (
    capability_revision IS NULL
    OR (capability_revision >= 1 AND change_kind IS NOT NULL AND actor_kind IS NOT NULL
        AND actor_ref IS NOT NULL AND reason_code IS NOT NULL AND source_observed_at IS NOT NULL
        AND (source <> 'PROVIDER_CONFIRMATION' OR status IN ('REVOKED', 'EXPIRED') OR binding_id IS NOT NULL)));

CREATE OR REPLACE FUNCTION number_guard_capability() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'capability history is never deleted' USING ERRCODE = '42501';
    END IF;
    IF OLD.effective_to IS NOT NULL
       OR (to_jsonb(NEW) - 'effective_to' - 'superseded_by') IS DISTINCT FROM
          (to_jsonb(OLD) - 'effective_to' - 'superseded_by') THEN
        RAISE EXCEPTION 'a capability record only ever closes (effective_to, superseded_by)' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION number_guard_resource() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    mode TEXT := current_setting('numbering.lifecycle', true);
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'number resources are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.state <> 'DISCOVERED' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'a number resource starts DISCOVERED at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.e164 IS DISTINCT FROM OLD.e164
       OR NEW.country_code IS DISTINCT FROM OLD.country_code OR NEW.calling_code IS DISTINCT FROM OLD.calling_code
       OR NEW.inventory_source IS DISTINCT FROM OLD.inventory_source
       OR NEW.discovered_at IS DISTINCT FROM OLD.discovered_at OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'the canonical identity of a number resource is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'number resource updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.binding_revision < OLD.binding_revision OR NEW.routing_revision < OLD.routing_revision
       OR NEW.capability_revision < OLD.capability_revision THEN
        RAISE EXCEPTION 'binding, routing and capability revisions never decrease' USING ERRCODE = '23514';
    END IF;
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF mode IS NULL OR mode NOT IN ('transition', 'reconciliation') THEN
            RAISE EXCEPTION 'number lifecycle state changes only through a domain transition' USING ERRCODE = '42501';
        END IF;
        IF NOT number_lifecycle_allowed(OLD.state, NEW.state) THEN
            RAISE EXCEPTION 'lifecycle transition % -> % is not allowed', OLD.state, NEW.state USING ERRCODE = '23514';
        END IF;
        IF OLD.state = 'UNKNOWN_PROVIDER_STATE' AND mode <> 'reconciliation' THEN
            RAISE EXCEPTION 'UNKNOWN_PROVIDER_STATE is resolved by reconciliation only' USING ERRCODE = '42501';
        END IF;
        IF NEW.state = 'ACTIVE' AND OLD.state = 'PROVISIONED' AND NOT EXISTS (
            SELECT 1 FROM dcn_assignments a WHERE a.number_resource_id = NEW.id AND a.status = 'ACTIVATING') THEN
            RAISE EXCEPTION 'a number resource becomes ACTIVE only for an ACTIVATING assignment' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END $$;
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_capabilities WHERE status IN ('EXPIRED', 'REVOKED')) THEN
        RAISE EXCEPTION 'capability records with EXPIRED / REVOKED status exist; rolling back would lose evidence';
    END IF;
END $$;

CREATE OR REPLACE FUNCTION number_guard_resource() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    mode TEXT := current_setting('numbering.lifecycle', true);
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'number resources are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.state <> 'DISCOVERED' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'a number resource starts DISCOVERED at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.e164 IS DISTINCT FROM OLD.e164
       OR NEW.country_code IS DISTINCT FROM OLD.country_code OR NEW.calling_code IS DISTINCT FROM OLD.calling_code
       OR NEW.inventory_source IS DISTINCT FROM OLD.inventory_source
       OR NEW.discovered_at IS DISTINCT FROM OLD.discovered_at OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'the canonical identity of a number resource is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'number resource updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.binding_revision < OLD.binding_revision OR NEW.routing_revision < OLD.routing_revision THEN
        RAISE EXCEPTION 'binding and routing revisions never decrease' USING ERRCODE = '23514';
    END IF;
    IF NEW.state IS DISTINCT FROM OLD.state THEN
        IF mode IS NULL OR mode NOT IN ('transition', 'reconciliation') THEN
            RAISE EXCEPTION 'number lifecycle state changes only through a domain transition' USING ERRCODE = '42501';
        END IF;
        IF NOT number_lifecycle_allowed(OLD.state, NEW.state) THEN
            RAISE EXCEPTION 'lifecycle transition % -> % is not allowed', OLD.state, NEW.state USING ERRCODE = '23514';
        END IF;
        IF OLD.state = 'UNKNOWN_PROVIDER_STATE' AND mode <> 'reconciliation' THEN
            RAISE EXCEPTION 'UNKNOWN_PROVIDER_STATE is resolved by reconciliation only' USING ERRCODE = '42501';
        END IF;
        IF NEW.state = 'ACTIVE' AND OLD.state = 'PROVISIONED' AND NOT EXISTS (
            SELECT 1 FROM dcn_assignments a WHERE a.number_resource_id = NEW.id AND a.status = 'ACTIVATING') THEN
            RAISE EXCEPTION 'a number resource becomes ACTIVE only for an ACTIVATING assignment' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION number_guard_capability() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'capability history is never deleted' USING ERRCODE = '42501';
    END IF;
    IF OLD.effective_to IS NOT NULL
       OR NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.capability IS DISTINCT FROM OLD.capability OR NEW.status IS DISTINCT FROM OLD.status
       OR NEW.source IS DISTINCT FROM OLD.source OR NEW.evidence_ref IS DISTINCT FROM OLD.evidence_ref
       OR NEW.verified_at IS DISTINCT FROM OLD.verified_at OR NEW.effective_from IS DISTINCT FROM OLD.effective_from
       OR NEW.limitations IS DISTINCT FROM OLD.limitations OR NEW.recorded_at IS DISTINCT FROM OLD.recorded_at THEN
        RAISE EXCEPTION 'a capability record only ever closes (effective_to, superseded_by)' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;

ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_provenance;
ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_reason;
ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_actor;
ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_change_kind;
ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_expiry;
ALTER TABLE number_capabilities DROP CONSTRAINT ck_number_capabilities_status;
ALTER TABLE number_capabilities ADD CONSTRAINT ck_number_capabilities_status CHECK (status IN
    ('VERIFIED', 'DECLARED', 'RESTRICTED', 'WITHDRAWN'));
ALTER TABLE number_capabilities DROP CONSTRAINT fk_number_capabilities_binding;
ALTER TABLE number_capabilities
    DROP COLUMN reason_code, DROP COLUMN actor_ref, DROP COLUMN actor_kind, DROP COLUMN change_kind,
    DROP COLUMN capability_revision, DROP COLUMN binding_id, DROP COLUMN source_observed_at, DROP COLUMN expires_at;
ALTER TABLE number_resources DROP CONSTRAINT ck_number_resources_capability_revision;
ALTER TABLE number_resources DROP COLUMN capability_revision;

DROP TABLE IF EXISTS number_type_catalog;
DROP TABLE IF EXISTS number_country_catalog;
DROP FUNCTION IF EXISTS number_guard_catalog_entry();
DROP TABLE IF EXISTS number_catalog_revisions;
DROP FUNCTION IF EXISTS number_guard_catalog_revision();
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
