//! The number domain (Part IV Workstream D): canonical Number Resources and their
//! lifecycle audit, provider bindings, capabilities, inventory observations, routing
//! profiles, reservations, DCN assignments and assignment history.
//!
//! The rules the domain enforces are ALSO enforced here, so no code path (including a
//! hand-written statement) can break them:
//!
//! * lifecycle: a resource's state changes only inside a domain transition
//!   (`SET LOCAL numbering.lifecycle = 'transition' | 'reconciliation'`), along an allowed
//!   edge (`number_lifecycle_allowed`), with `revision + 1`; UNKNOWN_PROVIDER_STATE is
//!   left only by reconciliation; ACTIVE needs an ACTIVATING assignment;
//! * bindings: at most one ACTIVE binding per routable capability of a resource
//!   (`ex_number_provider_bindings_active_capability`), synthetic resources bind only to
//!   the synthetic provider;
//! * capabilities: one current row per capability; VERIFIED needs evidence;
//! * reservations: one current reservation per resource; only AVAILABLE / REASSIGNABLE
//!   resources can be reserved (quarantined or uncertain ones never);
//! * assignments: one open (not CLOSED) assignment per resource; status changes only
//!   along `dcn_assignment_transition_allowed`; ACTIVE needs eligibility, a routing
//!   profile revision and an ACTIVE resource; ownership is immutable (a new owner is a new
//!   assignment);
//! * environment separation: SANDBOX / TEST scopes use SYNTHETIC_TEST inventory only,
//!   PRODUCTION never does;
//! * scope consistency: composite foreign keys pin routing profiles, reservations and
//!   history rows to the assignment's tenant / application / environment;
//! * history and lifecycle audit are append-only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Shared guards
-- ---------------------------------------------------------------------------
CREATE FUNCTION numbering_forbid_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = '42501';
END $$;

-- The canonical lifecycle edges (mirrors numbers::lifecycle::allowed; a test compares them).
CREATE FUNCTION number_lifecycle_allowed(from_state TEXT, to_state TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_state, to_state) IN (
        ('DISCOVERED', 'AVAILABLE'), ('DISCOVERED', 'FAILED'), ('DISCOVERED', 'RELEASED'),
        ('DISCOVERED', 'UNKNOWN_PROVIDER_STATE'),
        ('AVAILABLE', 'RESERVING'), ('AVAILABLE', 'QUARANTINED'), ('AVAILABLE', 'RELEASED'),
        ('AVAILABLE', 'UNKNOWN_PROVIDER_STATE'),
        ('RESERVING', 'RESERVED'), ('RESERVING', 'AVAILABLE'), ('RESERVING', 'REASSIGNABLE'),
        ('RESERVING', 'FAILED'), ('RESERVING', 'UNKNOWN_PROVIDER_STATE'),
        ('RESERVED', 'PROVISIONING'), ('RESERVED', 'AVAILABLE'), ('RESERVED', 'REASSIGNABLE'),
        ('RESERVED', 'UNKNOWN_PROVIDER_STATE'),
        ('PROVISIONING', 'PROVISIONED'), ('PROVISIONING', 'RESERVED'), ('PROVISIONING', 'FAILED'),
        ('PROVISIONING', 'UNKNOWN_PROVIDER_STATE'),
        ('PROVISIONED', 'ACTIVE'), ('PROVISIONED', 'RELEASING'),
        ('ACTIVE', 'SUSPENDED'), ('ACTIVE', 'RELEASING'),
        ('SUSPENDED', 'ACTIVE'), ('SUSPENDED', 'RELEASING'),
        ('RELEASING', 'QUARANTINED'), ('RELEASING', 'RELEASED'), ('RELEASING', 'FAILED'),
        ('RELEASING', 'UNKNOWN_PROVIDER_STATE'),
        ('QUARANTINED', 'REASSIGNABLE'), ('QUARANTINED', 'RELEASING'),
        ('REASSIGNABLE', 'RESERVING'), ('REASSIGNABLE', 'RELEASING'), ('REASSIGNABLE', 'QUARANTINED'),
        ('RELEASED', 'DISCOVERED'),
        ('FAILED', 'DISCOVERED'),
        ('UNKNOWN_PROVIDER_STATE', 'AVAILABLE'), ('UNKNOWN_PROVIDER_STATE', 'RESERVED'),
        ('UNKNOWN_PROVIDER_STATE', 'PROVISIONED'), ('UNKNOWN_PROVIDER_STATE', 'ACTIVE'),
        ('UNKNOWN_PROVIDER_STATE', 'QUARANTINED'), ('UNKNOWN_PROVIDER_STATE', 'REASSIGNABLE'),
        ('UNKNOWN_PROVIDER_STATE', 'RELEASED'), ('UNKNOWN_PROVIDER_STATE', 'FAILED')
    )
$$;

-- The DCN assignment edges (mirrors numbers::assignments::allowed).
CREATE FUNCTION dcn_assignment_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('REQUESTED', 'PENDING_ELIGIBILITY'), ('REQUESTED', 'CLOSED'),
        ('PENDING_ELIGIBILITY', 'ELIGIBLE'), ('PENDING_ELIGIBILITY', 'CLOSED'),
        ('ELIGIBLE', 'RESERVED'), ('ELIGIBLE', 'CLOSED'),
        ('RESERVED', 'ACTIVATING'), ('RESERVED', 'CLOSED'),
        ('ACTIVATING', 'ACTIVE'), ('ACTIVATING', 'RESERVED'), ('ACTIVATING', 'CLOSED'),
        ('ACTIVE', 'SUSPENDED'), ('ACTIVE', 'RELEASE_REQUESTED'),
        ('SUSPENDED', 'ACTIVE'), ('SUSPENDED', 'RELEASE_REQUESTED'),
        ('RELEASE_REQUESTED', 'ROUTING_REMOVED'),
        ('ROUTING_REMOVED', 'QUARANTINED'), ('ROUTING_REMOVED', 'CLOSED'),
        ('QUARANTINED', 'CLOSED')
    )
$$;

-- ---------------------------------------------------------------------------
-- Number Resources
-- ---------------------------------------------------------------------------
CREATE TABLE number_resources (
    id                      UUID         NOT NULL,
    e164                    VARCHAR(16)  NOT NULL,
    country_code            CHAR(2)      NOT NULL,
    calling_code            VARCHAR(3)   NOT NULL,
    region                  VARCHAR(100),
    number_type             VARCHAR(16)  NOT NULL DEFAULT 'UNCLASSIFIED',
    classification_source   VARCHAR(24)  NOT NULL DEFAULT 'NONE',
    inventory_source        VARCHAR(16)  NOT NULL,
    state                   VARCHAR(24)  NOT NULL DEFAULT 'DISCOVERED',
    revision                BIGINT       NOT NULL DEFAULT 1,
    state_changed_at        TIMESTAMPTZ  NOT NULL,
    discovered_at           TIMESTAMPTZ  NOT NULL,
    available_at            TIMESTAMPTZ,
    reserved_at             TIMESTAMPTZ,
    provisioned_at          TIMESTAMPTZ,
    activated_at            TIMESTAMPTZ,
    suspended_at            TIMESTAMPTZ,
    released_at             TIMESTAMPTZ,
    failed_at               TIMESTAMPTZ,
    quarantine_until        TIMESTAMPTZ,
    suspension_reason       VARCHAR(64),
    failure_category        VARCHAR(32),
    uncertain_operation     VARCHAR(32),
    uncertain_from_state    VARCHAR(24),
    uncertain_since         TIMESTAMPTZ,
    last_observed_at        TIMESTAMPTZ,
    observation_fresh_until TIMESTAMPTZ,
    binding_revision        BIGINT       NOT NULL DEFAULT 0,
    kyc_requirement_ref     UUID,
    kyc_profile_ref         UUID,
    jurisdiction_policy_ref UUID,
    risk_tier               VARCHAR(16)  NOT NULL DEFAULT 'UNASSESSED',
    restriction_flags       TEXT[]       NOT NULL DEFAULT '{}',
    voice_route_ref         UUID,
    sms_route_ref           UUID,
    routing_revision        BIGINT       NOT NULL DEFAULT 0,
    monthly_cost_minor      BIGINT,
    setup_cost_minor        BIGINT,
    currency                CHAR(3),
    cost_evidence           VARCHAR(16)  NOT NULL DEFAULT 'UNKNOWN',
    subscription_policy_ref UUID,
    usage_policy_ref        UUID,
    renewal_at              TIMESTAMPTZ,
    renewal_period_days     INTEGER,
    created_at              TIMESTAMPTZ  NOT NULL,
    updated_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_resources PRIMARY KEY (id),
    CONSTRAINT ck_number_resources_e164 CHECK (e164 ~ '^\+[1-9][0-9]{6,14}$'),
    CONSTRAINT ck_number_resources_country CHECK (country_code ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_number_resources_calling_code CHECK (calling_code ~ '^[1-9][0-9]{0,2}$'
        AND substr(e164, 2, length(calling_code)) = calling_code),
    CONSTRAINT ck_number_resources_type CHECK (number_type IN
        ('UNCLASSIFIED', 'GEOGRAPHIC', 'MOBILE', 'TOLL_FREE', 'NATIONAL')),
    CONSTRAINT ck_number_resources_classification_source CHECK (classification_source IN
        ('NONE', 'PROVIDER_INVENTORY', 'MANUAL_IMPORT', 'SYNTHETIC')),
    -- A classification is evidence-backed; synthetic inventory is classified synthetically.
    CONSTRAINT ck_number_resources_classified CHECK ((number_type = 'UNCLASSIFIED') = (classification_source = 'NONE')),
    CONSTRAINT ck_number_resources_synthetic_classification CHECK (
        (inventory_source = 'SYNTHETIC_TEST' AND classification_source IN ('NONE', 'SYNTHETIC'))
        OR (inventory_source <> 'SYNTHETIC_TEST' AND classification_source <> 'SYNTHETIC')),
    CONSTRAINT ck_number_resources_inventory_source CHECK (inventory_source IN
        ('LIVE_PROVIDER', 'SYNTHETIC_TEST', 'MANUAL_IMPORT')),
    CONSTRAINT ck_number_resources_state CHECK (state IN
        ('DISCOVERED', 'AVAILABLE', 'RESERVING', 'RESERVED', 'PROVISIONING', 'PROVISIONED', 'ACTIVE',
         'SUSPENDED', 'RELEASING', 'QUARANTINED', 'REASSIGNABLE', 'RELEASED', 'FAILED',
         'UNKNOWN_PROVIDER_STATE')),
    CONSTRAINT ck_number_resources_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_resources_uncertain CHECK ((state = 'UNKNOWN_PROVIDER_STATE') =
        (uncertain_operation IS NOT NULL AND uncertain_from_state IS NOT NULL AND uncertain_since IS NOT NULL)),
    CONSTRAINT ck_number_resources_uncertain_operation CHECK (uncertain_operation IS NULL OR uncertain_operation IN
        ('DISCOVER', 'RESERVE', 'RELEASE_HOLD', 'PROVISION', 'ACTIVATE', 'RELEASE', 'PORT')),
    CONSTRAINT ck_number_resources_quarantine CHECK (state <> 'QUARANTINED' OR quarantine_until IS NOT NULL),
    CONSTRAINT ck_number_resources_suspension CHECK ((state = 'SUSPENDED') = (suspension_reason IS NOT NULL)),
    CONSTRAINT ck_number_resources_failure CHECK ((state = 'FAILED') = (failure_category IS NOT NULL)),
    CONSTRAINT ck_number_resources_failure_category CHECK (failure_category IS NULL OR failure_category IN
        ('PROVIDER_REJECTED', 'PROVIDER_UNAVAILABLE', 'NOT_OBTAINABLE', 'POLICY_DENIED',
         'VALIDATION_FAILED', 'RECONCILED_FAILED')),
    CONSTRAINT ck_number_resources_freshness CHECK (observation_fresh_until IS NULL
        OR (last_observed_at IS NOT NULL AND observation_fresh_until > last_observed_at)),
    CONSTRAINT ck_number_resources_risk CHECK (risk_tier IN ('UNASSESSED', 'STANDARD', 'ELEVATED', 'HIGH')),
    CONSTRAINT ck_number_resources_restrictions CHECK (restriction_flags <@ ARRAY[
        'NO_INTERNATIONAL_OUTBOUND', 'NO_PREMIUM_DESTINATIONS', 'KYC_REQUIRED', 'LOCAL_PRESENCE_REQUIRED',
        'SMS_REGISTRATION_REQUIRED', 'EMERGENCY_SERVICES_UNSUPPORTED']::TEXT[]),
    CONSTRAINT ck_number_resources_cost CHECK (
        (monthly_cost_minor IS NULL OR monthly_cost_minor >= 0)
        AND (setup_cost_minor IS NULL OR setup_cost_minor >= 0)
        AND ((monthly_cost_minor IS NULL AND setup_cost_minor IS NULL) OR currency IS NOT NULL)
        AND (currency IS NULL OR currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_number_resources_cost_evidence CHECK (cost_evidence IN ('KNOWN', 'ESTIMATED', 'UNKNOWN')
        AND (cost_evidence <> 'UNKNOWN' OR (monthly_cost_minor IS NULL AND setup_cost_minor IS NULL))),
    CONSTRAINT ck_number_resources_renewal CHECK (renewal_period_days IS NULL OR renewal_period_days BETWEEN 1 AND 3660),
    CONSTRAINT ck_number_resources_revisions CHECK (binding_revision >= 0 AND routing_revision >= 0)
);
-- Canonical identity: a public number exists once among real inventory (live + imported)
-- and once among synthetic test inventory, which never collides with the real one.
CREATE UNIQUE INDEX uq_number_resources_real_e164 ON number_resources (e164)
    WHERE inventory_source <> 'SYNTHETIC_TEST';
CREATE UNIQUE INDEX uq_number_resources_synthetic_e164 ON number_resources (e164)
    WHERE inventory_source = 'SYNTHETIC_TEST';
CREATE INDEX ix_number_resources_state ON number_resources (state, country_code, inventory_source);

CREATE FUNCTION number_guard_resource() RETURNS trigger LANGUAGE plpgsql AS $$
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
CREATE TRIGGER trg_number_resources_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_resources
    FOR EACH ROW EXECUTE FUNCTION number_guard_resource();
CREATE TRIGGER trg_number_resources_no_truncate
    BEFORE TRUNCATE ON number_resources
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- Lifecycle audit (append-only): every transition, its actor, reason and evidence.
CREATE TABLE number_lifecycle_events (
    id                 UUID         NOT NULL,
    sequence           BIGSERIAL    NOT NULL,
    number_resource_id UUID         NOT NULL,
    from_state         VARCHAR(24),
    to_state           VARCHAR(24)  NOT NULL,
    revision           BIGINT       NOT NULL,
    actor_kind         VARCHAR(16)  NOT NULL,
    actor_ref          VARCHAR(200) NOT NULL,
    reason_code        VARCHAR(64)  NOT NULL,
    failure_category   VARCHAR(32),
    evidence_ref       UUID,
    correlation_id     VARCHAR(64)  NOT NULL,
    occurred_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_lifecycle_events PRIMARY KEY (id),
    CONSTRAINT uq_number_lifecycle_events_sequence UNIQUE (sequence),
    CONSTRAINT uq_number_lifecycle_events_revision UNIQUE (number_resource_id, revision),
    CONSTRAINT fk_number_lifecycle_events_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT ck_number_lifecycle_events_actor CHECK (actor_kind IN
        ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER')),
    CONSTRAINT ck_number_lifecycle_events_actor_ref CHECK (actor_ref ~ '^[A-Za-z0-9._:@-]{1,200}$'),
    CONSTRAINT ck_number_lifecycle_events_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_lifecycle_events_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_number_lifecycle_events_edge CHECK (from_state IS NULL OR number_lifecycle_allowed(from_state, to_state))
);
CREATE TRIGGER trg_number_lifecycle_events_append_only
    BEFORE UPDATE OR DELETE ON number_lifecycle_events
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_lifecycle_events_no_truncate
    BEFORE TRUNCATE ON number_lifecycle_events
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Provider bindings (internal: provider identifiers never leave Numbering)
-- ---------------------------------------------------------------------------
CREATE TABLE number_provider_bindings (
    id                       UUID         NOT NULL,
    number_resource_id       UUID         NOT NULL,
    provider_id              VARCHAR(64)  NOT NULL,
    external_ref             VARCHAR(200) NOT NULL,
    capabilities             TEXT[]       NOT NULL,
    status                   VARCHAR(16)  NOT NULL,
    binding_revision         BIGINT       NOT NULL,
    change_reason            VARCHAR(32)  NOT NULL,
    effective_from           TIMESTAMPTZ  NOT NULL,
    effective_to             TIMESTAMPTZ,
    superseded_by_binding_id UUID,
    created_at               TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_provider_bindings PRIMARY KEY (id),
    CONSTRAINT uq_number_provider_bindings_revision UNIQUE (number_resource_id, binding_revision),
    CONSTRAINT fk_number_provider_bindings_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_number_provider_bindings_successor FOREIGN KEY (superseded_by_binding_id) REFERENCES number_provider_bindings (id)
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT ck_number_provider_bindings_provider CHECK (provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_provider_bindings_external_ref CHECK (length(external_ref) BETWEEN 1 AND 200),
    CONSTRAINT ck_number_provider_bindings_capabilities CHECK (cardinality(capabilities) >= 1
        AND capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH']::TEXT[]),
    CONSTRAINT ck_number_provider_bindings_status CHECK (status IN ('ACTIVE', 'ENDED')),
    CONSTRAINT ck_number_provider_bindings_period CHECK ((status = 'ACTIVE') = (effective_to IS NULL)
        AND (effective_to IS NULL OR effective_to >= effective_from)),
    CONSTRAINT ck_number_provider_bindings_revision CHECK (binding_revision >= 1),
    CONSTRAINT ck_number_provider_bindings_reason CHECK (change_reason IN
        ('INITIAL', 'PROVIDER_REPLACEMENT', 'PORT_IN', 'PORT_OUT', 'CORRECTION', 'RELEASE'))
);
CREATE UNIQUE INDEX uq_number_provider_bindings_active_ref ON number_provider_bindings (provider_id, external_ref)
    WHERE status = 'ACTIVE';
CREATE INDEX ix_number_provider_bindings_resource ON number_provider_bindings (number_resource_id, status);

CREATE FUNCTION number_guard_binding() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    src TEXT;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'provider bindings are history and never deleted' USING ERRCODE = '42501';
    END IF;
    -- Serialize binding changes of one resource (exclusivity below is race-free).
    SELECT inventory_source INTO src FROM number_resources WHERE id = NEW.number_resource_id FOR UPDATE;
    IF TG_OP = 'UPDATE' THEN
        IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
           OR NEW.provider_id IS DISTINCT FROM OLD.provider_id OR NEW.external_ref IS DISTINCT FROM OLD.external_ref
           OR NEW.capabilities IS DISTINCT FROM OLD.capabilities OR NEW.binding_revision IS DISTINCT FROM OLD.binding_revision
           OR NEW.change_reason IS DISTINCT FROM OLD.change_reason OR NEW.effective_from IS DISTINCT FROM OLD.effective_from
           OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
            RAISE EXCEPTION 'a provider binding is immutable; end it and create a new one' USING ERRCODE = '42501';
        END IF;
        IF OLD.status = 'ENDED' THEN
            RAISE EXCEPTION 'an ENDED provider binding is history' USING ERRCODE = '42501';
        END IF;
    END IF;
    IF (src = 'SYNTHETIC_TEST') <> (NEW.provider_id = 'synthetic') THEN
        RAISE EXCEPTION 'synthetic inventory binds to the synthetic provider only, and only synthetic inventory does'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.status = 'ACTIVE' AND EXISTS (
        SELECT 1 FROM number_provider_bindings b
         WHERE b.number_resource_id = NEW.number_resource_id AND b.status = 'ACTIVE'
           AND b.id <> NEW.id AND b.capabilities && NEW.capabilities) THEN
        RAISE EXCEPTION 'conflicting active provider binding for a capability of this number'
            USING ERRCODE = '23P01', CONSTRAINT = 'ex_number_provider_bindings_active_capability';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_provider_bindings_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_provider_bindings
    FOR EACH ROW EXECUTE FUNCTION number_guard_binding();
CREATE TRIGGER trg_number_provider_bindings_no_truncate
    BEFORE TRUNCATE ON number_provider_bindings
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Capabilities (append-oriented: a change closes the current row and adds a new one)
-- ---------------------------------------------------------------------------
CREATE TABLE number_capabilities (
    id                 UUID         NOT NULL,
    number_resource_id UUID         NOT NULL,
    capability         VARCHAR(24)  NOT NULL,
    status             VARCHAR(16)  NOT NULL,
    source             VARCHAR(24)  NOT NULL,
    evidence_ref       UUID,
    verified_at        TIMESTAMPTZ,
    effective_from     TIMESTAMPTZ  NOT NULL,
    effective_to       TIMESTAMPTZ,
    limitations        JSONB        NOT NULL DEFAULT '{}'::jsonb,
    superseded_by      UUID,
    recorded_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_capabilities PRIMARY KEY (id),
    CONSTRAINT fk_number_capabilities_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_number_capabilities_successor FOREIGN KEY (superseded_by) REFERENCES number_capabilities (id)
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT ck_number_capabilities_capability CHECK (capability IN
        ('VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH',
         'MOBILE_NUMBER_CLASS', 'GEOGRAPHIC_LOCAL', 'TOLL_FREE_BUSINESS')),
    CONSTRAINT ck_number_capabilities_status CHECK (status IN ('VERIFIED', 'DECLARED', 'RESTRICTED', 'WITHDRAWN')),
    CONSTRAINT ck_number_capabilities_source CHECK (source IN
        ('PROVIDER_INVENTORY', 'PROVIDER_CONFIRMATION', 'MANUAL_IMPORT', 'SYNTHETIC')),
    -- Only evidence-backed facts are VERIFIED; synthetic inventory never is.
    CONSTRAINT ck_number_capabilities_verified CHECK (status <> 'VERIFIED'
        OR (evidence_ref IS NOT NULL AND verified_at IS NOT NULL AND source <> 'SYNTHETIC')),
    CONSTRAINT ck_number_capabilities_period CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_number_capabilities_limitations CHECK (jsonb_typeof(limitations) = 'object')
);
CREATE UNIQUE INDEX uq_number_capabilities_current ON number_capabilities (number_resource_id, capability)
    WHERE effective_to IS NULL;

CREATE FUNCTION number_guard_capability() RETURNS trigger LANGUAGE plpgsql AS $$
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
CREATE TRIGGER trg_number_capabilities_guard
    BEFORE UPDATE OR DELETE ON number_capabilities
    FOR EACH ROW EXECUTE FUNCTION number_guard_capability();
CREATE TRIGGER trg_number_capabilities_no_truncate
    BEFORE TRUNCATE ON number_capabilities
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Inventory observations (append-only). An observation is not an allocation.
-- ---------------------------------------------------------------------------
CREATE TABLE number_inventory_snapshots (
    id                   UUID         NOT NULL,
    -- Insertion order: the tie-breaker between observations made at the same instant.
    sequence             BIGINT       GENERATED ALWAYS AS IDENTITY,
    number_resource_id   UUID         NOT NULL,
    source               VARCHAR(16)  NOT NULL,
    provider_id          VARCHAR(64),
    observed_at          TIMESTAMPTZ  NOT NULL,
    fresh_until          TIMESTAMPTZ  NOT NULL,
    availability         VARCHAR(16)  NOT NULL,
    offered_capabilities TEXT[]       NOT NULL DEFAULT '{}',
    observed_number_type VARCHAR(16),
    monthly_cost_minor   BIGINT,
    setup_cost_minor     BIGINT,
    currency             CHAR(3),
    evidence_ref         UUID,
    query_ref            UUID,
    recorded_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_inventory_snapshots PRIMARY KEY (id),
    CONSTRAINT fk_number_inventory_snapshots_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT ck_number_inventory_snapshots_source CHECK (source IN ('LIVE_PROVIDER', 'SYNTHETIC_TEST', 'MANUAL_IMPORT')),
    CONSTRAINT ck_number_inventory_snapshots_provider CHECK (
        (source = 'LIVE_PROVIDER' AND provider_id IS NOT NULL AND provider_id <> 'synthetic')
        OR (source = 'SYNTHETIC_TEST' AND (provider_id IS NULL OR provider_id = 'synthetic'))
        OR (source = 'MANUAL_IMPORT' AND (provider_id IS NULL OR provider_id <> 'synthetic'))),
    CONSTRAINT ck_number_inventory_snapshots_provider_format CHECK (provider_id IS NULL OR provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_inventory_snapshots_freshness CHECK (fresh_until > observed_at),
    CONSTRAINT ck_number_inventory_snapshots_availability CHECK (availability IN ('AVAILABLE', 'UNAVAILABLE', 'UNKNOWN')),
    CONSTRAINT ck_number_inventory_snapshots_capabilities CHECK (offered_capabilities <@ ARRAY[
        'VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH',
        'MOBILE_NUMBER_CLASS', 'GEOGRAPHIC_LOCAL', 'TOLL_FREE_BUSINESS']::TEXT[]),
    CONSTRAINT ck_number_inventory_snapshots_type CHECK (observed_number_type IS NULL OR observed_number_type IN
        ('GEOGRAPHIC', 'MOBILE', 'TOLL_FREE', 'NATIONAL')),
    CONSTRAINT ck_number_inventory_snapshots_cost CHECK (
        ((monthly_cost_minor IS NULL AND setup_cost_minor IS NULL) OR currency IS NOT NULL)
        AND (monthly_cost_minor IS NULL OR monthly_cost_minor >= 0) AND (setup_cost_minor IS NULL OR setup_cost_minor >= 0))
);
CREATE INDEX ix_number_inventory_snapshots_resource ON number_inventory_snapshots (number_resource_id, observed_at DESC, sequence DESC);

CREATE FUNCTION number_guard_snapshot_source() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    src TEXT;
BEGIN
    SELECT inventory_source INTO src FROM number_resources WHERE id = NEW.number_resource_id;
    IF (src = 'SYNTHETIC_TEST') <> (NEW.source = 'SYNTHETIC_TEST') THEN
        RAISE EXCEPTION 'synthetic observations describe synthetic inventory only' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_inventory_snapshots_source
    BEFORE INSERT ON number_inventory_snapshots
    FOR EACH ROW EXECUTE FUNCTION number_guard_snapshot_source();
CREATE TRIGGER trg_number_inventory_snapshots_append_only
    BEFORE UPDATE OR DELETE ON number_inventory_snapshots
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_inventory_snapshots_no_truncate
    BEFORE TRUNCATE ON number_inventory_snapshots
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Routing profiles (versioned; targets are opaque CF Meet ids of other services)
-- ---------------------------------------------------------------------------
CREATE TABLE number_routing_profiles (
    id                UUID         NOT NULL,
    profile_id        UUID         NOT NULL,
    revision          BIGINT       NOT NULL,
    tenant_id         UUID         NOT NULL,
    application_id    UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    voice_target_kind VARCHAR(32)  NOT NULL,
    voice_target_ref  UUID,
    sms_target_kind   VARCHAR(32)  NOT NULL,
    sms_target_ref    UUID,
    status            VARCHAR(16)  NOT NULL,
    created_by        VARCHAR(200) NOT NULL,
    created_at        TIMESTAMPTZ  NOT NULL,
    superseded_at     TIMESTAMPTZ,
    CONSTRAINT pk_number_routing_profiles PRIMARY KEY (id),
    CONSTRAINT uq_number_routing_profiles_revision UNIQUE (profile_id, revision),
    CONSTRAINT uq_number_routing_profiles_scope UNIQUE (profile_id, revision, tenant_id, application_id, environment_id),
    CONSTRAINT ck_number_routing_profiles_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_routing_profiles_voice CHECK (voice_target_kind IN
        ('NONE', 'PRINCIPAL', 'CALLING_ENDPOINT', 'ENTERPRISE_EXTENSION', 'ENTERPRISE_RING_GROUP',
         'ENTERPRISE_QUEUE', 'ENTERPRISE_IVR', 'ENTERPRISE_DEPARTMENT')
        AND (voice_target_kind = 'NONE') = (voice_target_ref IS NULL)),
    CONSTRAINT ck_number_routing_profiles_sms CHECK (sms_target_kind IN ('NONE', 'PRINCIPAL', 'APPLICATION')
        AND (sms_target_kind = 'NONE') = (sms_target_ref IS NULL)),
    CONSTRAINT ck_number_routing_profiles_status CHECK (status IN ('CURRENT', 'SUPERSEDED')
        AND (status = 'SUPERSEDED') = (superseded_at IS NOT NULL)),
    CONSTRAINT ck_number_routing_profiles_created_by CHECK (created_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
CREATE UNIQUE INDEX uq_number_routing_profiles_current ON number_routing_profiles (profile_id) WHERE status = 'CURRENT';
CREATE INDEX ix_number_routing_profiles_scope ON number_routing_profiles (tenant_id, application_id, environment_id);

CREATE FUNCTION number_guard_routing_profile() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    latest RECORD;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'routing profile revisions are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'UPDATE' THEN
        IF OLD.status <> 'CURRENT' OR NEW.status <> 'SUPERSEDED'
           OR (to_jsonb(NEW) - 'status' - 'superseded_at') IS DISTINCT FROM (to_jsonb(OLD) - 'status' - 'superseded_at') THEN
            RAISE EXCEPTION 'a routing profile revision is immutable; it can only be superseded' USING ERRCODE = '42501';
        END IF;
        RETURN NEW;
    END IF;
    -- Serialize revisions of one profile; a new revision continues the previous one in
    -- the same scope.
    PERFORM pg_advisory_xact_lock(hashtextextended(NEW.profile_id::text, 7239101));
    SELECT revision, tenant_id, application_id, environment_id INTO latest
      FROM number_routing_profiles WHERE profile_id = NEW.profile_id ORDER BY revision DESC LIMIT 1;
    IF FOUND THEN
        IF NEW.revision <> latest.revision + 1 THEN
            RAISE EXCEPTION 'routing profile revision % does not follow %', NEW.revision, latest.revision USING ERRCODE = '40001';
        END IF;
        IF (NEW.tenant_id, NEW.application_id, NEW.environment_id)
           IS DISTINCT FROM (latest.tenant_id, latest.application_id, latest.environment_id) THEN
            RAISE EXCEPTION 'a routing profile never changes scope' USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.revision <> 1 THEN
        RAISE EXCEPTION 'a routing profile starts at revision 1' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_routing_profiles_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_routing_profiles
    FOR EACH ROW EXECUTE FUNCTION number_guard_routing_profile();
CREATE TRIGGER trg_number_routing_profiles_no_truncate
    BEFORE TRUNCATE ON number_routing_profiles
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Environment separation (shared by reservations and assignments)
-- ---------------------------------------------------------------------------
CREATE FUNCTION number_guard_environment_inventory(resource_id UUID, kind TEXT) RETURNS VOID
LANGUAGE plpgsql AS $$
DECLARE
    src TEXT;
BEGIN
    SELECT inventory_source INTO src FROM number_resources WHERE id = resource_id;
    IF kind = 'PRODUCTION' AND src = 'SYNTHETIC_TEST' THEN
        RAISE EXCEPTION 'synthetic test inventory is never used by a PRODUCTION environment' USING ERRCODE = '23514';
    END IF;
    IF kind IN ('SANDBOX', 'TEST') AND src <> 'SYNTHETIC_TEST' THEN
        RAISE EXCEPTION 'SANDBOX and TEST environments use synthetic test inventory only' USING ERRCODE = '23514';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- Reservations
-- ---------------------------------------------------------------------------
CREATE TABLE number_reservations (
    id                     UUID         NOT NULL,
    number_resource_id     UUID         NOT NULL,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    environment_kind       VARCHAR(16)  NOT NULL,
    service_account_id     UUID         NOT NULL,
    requested_for_principal_id UUID,
    status                 VARCHAR(16)  NOT NULL,
    revision               BIGINT       NOT NULL DEFAULT 1,
    expires_at             TIMESTAMPTZ  NOT NULL,
    provider_hold_ref      UUID,
    provider_release_state VARCHAR(16)  NOT NULL DEFAULT 'NOT_STARTED',
    release_evidence_ref   UUID,
    correlation_id         VARCHAR(64)  NOT NULL,
    created_at             TIMESTAMPTZ  NOT NULL,
    updated_at             TIMESTAMPTZ  NOT NULL,
    ended_at               TIMESTAMPTZ,
    CONSTRAINT pk_number_reservations PRIMARY KEY (id),
    CONSTRAINT uq_number_reservations_scope UNIQUE (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT fk_number_reservations_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT ck_number_reservations_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_number_reservations_status CHECK (status IN
        ('PENDING', 'HELD', 'UNKNOWN', 'EXPIRED', 'CANCELLED', 'CONVERTED', 'FAILED')),
    CONSTRAINT ck_number_reservations_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_reservations_expiry CHECK (expires_at > created_at),
    CONSTRAINT ck_number_reservations_ended CHECK ((status IN ('PENDING', 'HELD', 'UNKNOWN')) = (ended_at IS NULL)),
    CONSTRAINT ck_number_reservations_held CHECK (status <> 'HELD' OR provider_hold_ref IS NOT NULL),
    CONSTRAINT ck_number_reservations_release CHECK (provider_release_state IN
        ('NOT_STARTED', 'NOT_REQUIRED', 'PENDING', 'CONFIRMED', 'UNKNOWN')
        AND (provider_release_state <> 'CONFIRMED' OR release_evidence_ref IS NOT NULL)),
    CONSTRAINT ck_number_reservations_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
-- One current reservation per number.
CREATE UNIQUE INDEX uq_number_reservations_current ON number_reservations (number_resource_id)
    WHERE status IN ('PENDING', 'HELD', 'UNKNOWN');
CREATE INDEX ix_number_reservations_scope ON number_reservations (tenant_id, application_id, environment_id, created_at);
CREATE INDEX ix_number_reservations_expiry ON number_reservations (expires_at) WHERE status IN ('PENDING', 'HELD');

CREATE FUNCTION number_guard_reservation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    resource_state TEXT;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'reservations are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        SELECT state INTO resource_state FROM number_resources WHERE id = NEW.number_resource_id FOR UPDATE;
        IF resource_state NOT IN ('AVAILABLE', 'REASSIGNABLE') THEN
            RAISE EXCEPTION 'a % number resource cannot be reserved', resource_state USING ERRCODE = '23514';
        END IF;
        IF NEW.status <> 'PENDING' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'a reservation starts PENDING at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id OR NEW.environment_kind IS DISTINCT FROM OLD.environment_kind
       OR NEW.service_account_id IS DISTINCT FROM OLD.service_account_id
       OR NEW.requested_for_principal_id IS DISTINCT FROM OLD.requested_for_principal_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id THEN
        RAISE EXCEPTION 'the ownership of a reservation is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'reservation updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF OLD.status NOT IN ('PENDING', 'HELD', 'UNKNOWN') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % reservation is final', OLD.status USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_reservations_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_reservations
    FOR EACH ROW EXECUTE FUNCTION number_guard_reservation();
CREATE TRIGGER trg_number_reservations_no_truncate
    BEFORE TRUNCATE ON number_reservations
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- DCN assignments (separate from the resource; ownership immutable per record)
-- ---------------------------------------------------------------------------
CREATE TABLE dcn_assignments (
    id                       UUID         NOT NULL,
    number_resource_id       UUID         NOT NULL,
    reservation_id           UUID,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    environment_kind         VARCHAR(16)  NOT NULL,
    owner_kind               VARCHAR(16)  NOT NULL,
    owner_ref                UUID         NOT NULL,
    ownership_class          VARCHAR(24)  NOT NULL,
    routing_profile_id       UUID,
    routing_profile_revision BIGINT,
    capability_snapshot      TEXT[]       NOT NULL DEFAULT '{}',
    policy_snapshot_ref      UUID,
    eligibility_decision_ref UUID,
    kyc_profile_ref          UUID,
    risk_tier_snapshot       VARCHAR(16),
    status                   VARCHAR(24)  NOT NULL,
    revision                 BIGINT       NOT NULL DEFAULT 1,
    requested_at             TIMESTAMPTZ  NOT NULL,
    eligible_at              TIMESTAMPTZ,
    activated_at             TIMESTAMPTZ,
    suspended_at             TIMESTAMPTZ,
    release_requested_at     TIMESTAMPTZ,
    closed_at                TIMESTAMPTZ,
    close_reason             VARCHAR(64),
    created_by               VARCHAR(200) NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_dcn_assignments PRIMARY KEY (id),
    CONSTRAINT uq_dcn_assignments_scope UNIQUE (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT fk_dcn_assignments_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_dcn_assignments_reservation FOREIGN KEY (reservation_id, tenant_id, application_id, environment_id, number_resource_id)
        REFERENCES number_reservations (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT fk_dcn_assignments_routing FOREIGN KEY (routing_profile_id, routing_profile_revision, tenant_id, application_id, environment_id)
        REFERENCES number_routing_profiles (profile_id, revision, tenant_id, application_id, environment_id),
    CONSTRAINT ck_dcn_assignments_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_dcn_assignments_owner_kind CHECK (owner_kind IN ('PRINCIPAL', 'ORGANIZATION')),
    CONSTRAINT ck_dcn_assignments_class CHECK (ownership_class IN
        ('PERSONAL', 'PROFESSIONAL', 'ORGANIZATION', 'EMPLOYEE_DIRECT', 'DEPARTMENT', 'CAMPAIGN', 'SHARED_TEAM')),
    -- Personal / professional numbers belong to a principal; every organizational class
    -- (including a number routed to an employee) remains the organization's property.
    CONSTRAINT ck_dcn_assignments_owner_class CHECK (
        (ownership_class IN ('PERSONAL', 'PROFESSIONAL')) = (owner_kind = 'PRINCIPAL')),
    CONSTRAINT ck_dcn_assignments_status CHECK (status IN
        ('REQUESTED', 'PENDING_ELIGIBILITY', 'ELIGIBLE', 'RESERVED', 'ACTIVATING', 'ACTIVE', 'SUSPENDED',
         'RELEASE_REQUESTED', 'ROUTING_REMOVED', 'QUARANTINED', 'CLOSED')),
    CONSTRAINT ck_dcn_assignments_revision CHECK (revision >= 1),
    CONSTRAINT ck_dcn_assignments_capabilities CHECK (capability_snapshot <@ ARRAY[
        'VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH',
        'MOBILE_NUMBER_CLASS', 'GEOGRAPHIC_LOCAL', 'TOLL_FREE_BUSINESS']::TEXT[]),
    -- Eligibility is never bypassed: from ELIGIBLE on there is a recorded decision.
    CONSTRAINT ck_dcn_assignments_eligibility CHECK (status IN ('REQUESTED', 'PENDING_ELIGIBILITY', 'CLOSED')
        OR (eligibility_decision_ref IS NOT NULL AND eligible_at IS NOT NULL)),
    CONSTRAINT ck_dcn_assignments_reserved CHECK (status NOT IN ('RESERVED', 'ACTIVATING', 'ACTIVE', 'SUSPENDED')
        OR reservation_id IS NOT NULL),
    CONSTRAINT ck_dcn_assignments_routing CHECK ((routing_profile_id IS NULL) = (routing_profile_revision IS NULL)
        AND (status NOT IN ('ACTIVATING', 'ACTIVE', 'SUSPENDED') OR routing_profile_id IS NOT NULL)),
    CONSTRAINT ck_dcn_assignments_active CHECK (status NOT IN ('ACTIVE', 'SUSPENDED') OR activated_at IS NOT NULL),
    CONSTRAINT ck_dcn_assignments_closed CHECK ((status = 'CLOSED') = (closed_at IS NOT NULL AND close_reason IS NOT NULL)),
    CONSTRAINT ck_dcn_assignments_risk CHECK (risk_tier_snapshot IS NULL OR risk_tier_snapshot IN
        ('UNASSESSED', 'STANDARD', 'ELEVATED', 'HIGH')),
    CONSTRAINT ck_dcn_assignments_created_by CHECK (created_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
-- One open ownership relationship per number.
CREATE UNIQUE INDEX uq_dcn_assignments_open ON dcn_assignments (number_resource_id) WHERE status <> 'CLOSED';
CREATE INDEX ix_dcn_assignments_scope ON dcn_assignments (tenant_id, application_id, environment_id, requested_at);
CREATE INDEX ix_dcn_assignments_owner ON dcn_assignments (tenant_id, owner_kind, owner_ref);

CREATE FUNCTION dcn_guard_assignment() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    resource_state TEXT;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'assignments are never deleted (reassignment creates a new record)' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        IF NEW.status <> 'REQUESTED' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'an assignment starts REQUESTED at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id OR NEW.environment_kind IS DISTINCT FROM OLD.environment_kind
       OR NEW.owner_kind IS DISTINCT FROM OLD.owner_kind OR NEW.owner_ref IS DISTINCT FROM OLD.owner_ref
       OR NEW.ownership_class IS DISTINCT FROM OLD.ownership_class OR NEW.requested_at IS DISTINCT FROM OLD.requested_at
       OR NEW.created_by IS DISTINCT FROM OLD.created_by THEN
        RAISE EXCEPTION 'assignment ownership is immutable; a new owner is a new assignment' USING ERRCODE = '42501';
    END IF;
    IF OLD.reservation_id IS NOT NULL AND NEW.reservation_id IS DISTINCT FROM OLD.reservation_id THEN
        RAISE EXCEPTION 'an assignment keeps its reservation' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'assignment updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF NEW.status IS DISTINCT FROM OLD.status THEN
        IF current_setting('numbering.assignment', true) IS DISTINCT FROM 'transition' THEN
            RAISE EXCEPTION 'assignment status changes only through a domain transition' USING ERRCODE = '42501';
        END IF;
        IF NOT dcn_assignment_transition_allowed(OLD.status, NEW.status) THEN
            RAISE EXCEPTION 'assignment transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
        END IF;
        IF NEW.status = 'ACTIVE' THEN
            SELECT state INTO resource_state FROM number_resources WHERE id = NEW.number_resource_id;
            IF resource_state <> 'ACTIVE' THEN
                RAISE EXCEPTION 'an assignment becomes ACTIVE only on an ACTIVE number resource' USING ERRCODE = '23514';
            END IF;
        END IF;
    ELSIF OLD.status = 'CLOSED' THEN
        RAISE EXCEPTION 'a CLOSED assignment is history' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_dcn_assignments_guard
    BEFORE INSERT OR UPDATE OR DELETE ON dcn_assignments
    FOR EACH ROW EXECUTE FUNCTION dcn_guard_assignment();
CREATE TRIGGER trg_dcn_assignments_no_truncate
    BEFORE TRUNCATE ON dcn_assignments
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Assignment history (append-only)
-- ---------------------------------------------------------------------------
CREATE TABLE number_assignment_history (
    id                       UUID         NOT NULL,
    sequence                 BIGSERIAL    NOT NULL,
    assignment_id            UUID         NOT NULL,
    number_resource_id       UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    event_kind               VARCHAR(24)  NOT NULL,
    from_status              VARCHAR(24),
    to_status                VARCHAR(24)  NOT NULL,
    assignment_revision      BIGINT       NOT NULL,
    owner_kind               VARCHAR(16)  NOT NULL,
    owner_ref                UUID         NOT NULL,
    ownership_class          VARCHAR(24)  NOT NULL,
    routing_profile_id       UUID,
    routing_profile_revision BIGINT,
    capability_snapshot      TEXT[]       NOT NULL,
    eligibility_decision_ref UUID,
    kyc_profile_ref          UUID,
    risk_tier                VARCHAR(16),
    effective_from           TIMESTAMPTZ  NOT NULL,
    actor_kind               VARCHAR(16)  NOT NULL,
    actor_ref                VARCHAR(200) NOT NULL,
    reason_code              VARCHAR(64)  NOT NULL,
    correlation_id           VARCHAR(64)  NOT NULL,
    occurred_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_assignment_history PRIMARY KEY (id),
    CONSTRAINT uq_number_assignment_history_sequence UNIQUE (sequence),
    CONSTRAINT uq_number_assignment_history_revision UNIQUE (assignment_id, assignment_revision),
    CONSTRAINT fk_number_assignment_history_assignment FOREIGN KEY (assignment_id, tenant_id, application_id, environment_id, number_resource_id)
        REFERENCES dcn_assignments (id, tenant_id, application_id, environment_id, number_resource_id),
    CONSTRAINT ck_number_assignment_history_event CHECK (event_kind IN
        ('CREATED', 'STATUS_CHANGED', 'ROUTING_CHANGED', 'ELIGIBILITY_RECORDED')),
    CONSTRAINT ck_number_assignment_history_edge CHECK (from_status IS NULL OR from_status = to_status
        OR dcn_assignment_transition_allowed(from_status, to_status)),
    CONSTRAINT ck_number_assignment_history_actor CHECK (actor_kind IN
        ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER')),
    CONSTRAINT ck_number_assignment_history_actor_ref CHECK (actor_ref ~ '^[A-Za-z0-9._:@-]{1,200}$'),
    CONSTRAINT ck_number_assignment_history_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_assignment_history_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE INDEX ix_number_assignment_history_resource ON number_assignment_history (number_resource_id, sequence);
CREATE INDEX ix_number_assignment_history_scope ON number_assignment_history (tenant_id, application_id, environment_id, sequence);
CREATE TRIGGER trg_number_assignment_history_append_only
    BEFORE UPDATE OR DELETE ON number_assignment_history
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_assignment_history_no_truncate
    BEFORE TRUNCATE ON number_assignment_history
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS number_assignment_history;
DROP TABLE IF EXISTS dcn_assignments;
DROP FUNCTION IF EXISTS dcn_guard_assignment();
DROP TABLE IF EXISTS number_reservations;
DROP FUNCTION IF EXISTS number_guard_reservation();
DROP FUNCTION IF EXISTS number_guard_environment_inventory(UUID, TEXT);
DROP TABLE IF EXISTS number_routing_profiles;
DROP FUNCTION IF EXISTS number_guard_routing_profile();
DROP TABLE IF EXISTS number_inventory_snapshots;
DROP FUNCTION IF EXISTS number_guard_snapshot_source();
DROP TABLE IF EXISTS number_capabilities;
DROP FUNCTION IF EXISTS number_guard_capability();
DROP TABLE IF EXISTS number_provider_bindings;
DROP FUNCTION IF EXISTS number_guard_binding();
DROP TABLE IF EXISTS number_lifecycle_events;
DROP TABLE IF EXISTS number_resources;
DROP FUNCTION IF EXISTS number_guard_resource();
DROP FUNCTION IF EXISTS dcn_assignment_transition_allowed(TEXT, TEXT);
DROP FUNCTION IF EXISTS number_lifecycle_allowed(TEXT, TEXT);
DROP FUNCTION IF EXISTS numbering_forbid_mutation();
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
