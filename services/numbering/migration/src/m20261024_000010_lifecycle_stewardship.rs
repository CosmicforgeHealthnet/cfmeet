//! Part IV Workstreams P–X: lifecycle stewardship of a Digital Communications Number after
//! activation, and the Part III relay-number authority moving into Numbering.
//!
//! * `number_suspensions` (P) — who suspended a number, why, with what evidence; the
//!   provider suspension / restoration commands; the restoration checks; one open
//!   suspension per number.
//! * `number_release_requests` (Q) — an auditable release: requester, reason, owner and
//!   assignment, effective date, policy (`POOL_RETURN` keeps the number at the provider,
//!   `PROVIDER_RELINQUISH` gives it back), routing-removal and provider evidence. A release
//!   is never a delete.
//! * `number_quarantines` / `number_late_activity` (Q) — the cooldown with its policy,
//!   review / extension decisions, provider-state confirmation and completion; late voice /
//!   SMS traffic observed (and refused) while a number is releasing or quarantined.
//! * `number_history_access` (R) — every privileged history / ownership-verification read.
//! * `number_usage_records` (S) — idempotent internal cost / usage evidence (provider
//!   costs, never customer prices), provisional then final.
//! * `number_risk_signals`, `number_risk_decisions` (T) — append-only signals and
//!   explainable decisions (allocation, scoring, restriction, appeals).
//! * `number_port_requests`, `number_port_events` (U) — provider-independent port-in /
//!   port-out requests.
//! * `number_operational_controls`, `number_control_events` (W) — emergency kill switches
//!   with an append-only audit.
//! * `number_relay_pool_members`, `number_relay_leases` (Part P §§119–120) — Privacy Relay
//!   relay numbers as Number Resources of service class `PRIVACY_RELAY`, lent by temporary
//!   leases; never reservable or assignable as DCNs.
//!
//! The database enforces: one open suspension / release / quarantine per number; a
//! number is REASSIGNABLE only after a COMPLETED, non-relinquished quarantine with no
//! open release; relay-class numbers are never reserved or assigned; append-only
//! evidence stays append-only.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261024_000010_lifecycle_stewardship"
    }
}

const UP: &str = r#"
-- Event types of the specification (`number.suspended`, `number.restored`) have two segments.
ALTER TABLE numbering_outbox_events DROP CONSTRAINT ck_numbering_outbox_events_type;
ALTER TABLE numbering_outbox_events ADD CONSTRAINT ck_numbering_outbox_events_type
    CHECK (event_type ~ '^number(\.[a-z][a-z0-9-]*){1,}$' AND length(event_type) <= 80);

-- Incidents of the lifecycle workflows (P–W).
ALTER TABLE numbering_incidents DROP CONSTRAINT ck_numbering_incidents_category;
ALTER TABLE numbering_incidents ADD CONSTRAINT ck_numbering_incidents_category CHECK (category IN (
    'PROVIDER_STATE_DRIFT', 'UNKNOWN_PROVIDER_OUTCOME',
        'CALLBACK_AUTHENTICATION_FAILED', 'UNMATCHED_PROVIDER_EVENT', 'PROVIDER_RELEASED_UNEXPECTEDLY',
        'RECONCILIATION_CONFLICT', 'PROVIDER_OUTAGE', 'LIFECYCLE_SAFETY', 'QUARANTINE_CONFLICT', 'PROVIDER_COST_SPIKE', 'RISK_RESTRICTION'));

-- Service class: a DCN, or a Privacy Relay relay number (Part III inventory).
ALTER TABLE number_resources ADD COLUMN service_class VARCHAR(16) NOT NULL DEFAULT 'DCN';
ALTER TABLE number_resources ADD CONSTRAINT ck_number_resources_service_class
    CHECK (service_class IN ('DCN', 'PRIVACY_RELAY'));

CREATE FUNCTION number_guard_service_class() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.service_class IS DISTINCT FROM OLD.service_class THEN
        IF NOT (OLD.service_class = 'DCN' AND NEW.service_class = 'PRIVACY_RELAY'
                AND OLD.state IN ('DISCOVERED', 'AVAILABLE')
                AND NOT EXISTS (SELECT 1 FROM number_reservations r WHERE r.number_resource_id = OLD.id)
                AND NOT EXISTS (SELECT 1 FROM dcn_assignments a WHERE a.number_resource_id = OLD.id)) THEN
            RAISE EXCEPTION 'a service class changes only from DCN to PRIVACY_RELAY on an unused number' USING ERRCODE = '42501';
        END IF;
    END IF;
    -- Reassignment only after a completed, non-relinquished quarantine and no open release.
    IF NEW.state = 'REASSIGNABLE' AND OLD.state IS DISTINCT FROM NEW.state THEN
        IF EXISTS (SELECT 1 FROM number_release_requests q WHERE q.number_resource_id = NEW.id
                     AND q.status NOT IN ('QUARANTINED', 'FAILED', 'CANCELLED')) THEN
            RAISE EXCEPTION 'a number with an open release is not reassignable' USING ERRCODE = '23514';
        END IF;
        IF EXISTS (SELECT 1 FROM number_quarantines q WHERE q.number_resource_id = NEW.id
                     AND (q.status <> 'COMPLETED' OR q.disposition = 'RELINQUISHED'
                          OR q.provider_confirmed_at IS NULL)) THEN
            RAISE EXCEPTION 'a number is reassignable only after a completed, provider-confirmed quarantine' USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_resources_service_class
    BEFORE UPDATE ON number_resources
    FOR EACH ROW EXECUTE FUNCTION number_guard_service_class();

CREATE FUNCTION number_guard_dcn_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_resources r WHERE r.id = NEW.number_resource_id AND r.service_class <> 'DCN') THEN
        RAISE EXCEPTION 'relay-class numbers are never reserved or assigned as DCNs' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_reservations_dcn_only
    BEFORE INSERT ON number_reservations
    FOR EACH ROW EXECUTE FUNCTION number_guard_dcn_only();
CREATE TRIGGER trg_dcn_assignments_dcn_only
    BEFORE INSERT ON dcn_assignments
    FOR EACH ROW EXECUTE FUNCTION number_guard_dcn_only();

-- Shared guard: identity / evidence columns of a workflow record never change and the
-- record is never deleted; only the listed mutable columns move.
CREATE FUNCTION number_guard_workflow() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    mutable TEXT[] := TG_ARGV;
    n JSONB;
    o JSONB;
    c TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION '% records are never deleted', TG_TABLE_NAME USING ERRCODE = '42501';
    END IF;
    n := to_jsonb(NEW);
    o := to_jsonb(OLD);
    FOREACH c IN ARRAY mutable LOOP
        n := n - c;
        o := o - c;
    END LOOP;
    IF n IS DISTINCT FROM o THEN
        RAISE EXCEPTION '% identity and evidence columns are immutable', TG_TABLE_NAME USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;

-- P: suspension and restoration ---------------------------------------------------------
CREATE TABLE number_suspensions (
    id                  UUID         NOT NULL,
    number_resource_id  UUID         NOT NULL REFERENCES number_resources (id),
    assignment_id       UUID         NOT NULL REFERENCES dcn_assignments (id),
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    source              VARCHAR(16)  NOT NULL,
    reason_code         VARCHAR(64)  NOT NULL,
    evidence_ref        UUID,
    note_ref            UUID,
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_ref           VARCHAR(200) NOT NULL,
    suspended_at        TIMESTAMPTZ  NOT NULL,
    provider_command_id UUID,
    provider_state      VARCHAR(16)  NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    revision            BIGINT       NOT NULL DEFAULT 1,
    restore_requested_at TIMESTAMPTZ,
    restore_actor_kind  VARCHAR(16),
    restore_actor_ref   VARCHAR(200),
    restore_command_id  UUID,
    restore_checks      JSONB,
    restore_blocked_reason VARCHAR(64),
    restored_at         TIMESTAMPTZ,
    ended_reason        VARCHAR(64),
    updated_at          TIMESTAMPTZ  NOT NULL,
    correlation_id      VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_suspensions PRIMARY KEY (id),
    CONSTRAINT ck_number_suspensions_source CHECK (source IN ('CUSTOMER', 'ORGANIZATION', 'OPERATOR', 'RISK', 'PROVIDER')),
    CONSTRAINT ck_number_suspensions_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_suspensions_actor CHECK (actor_kind IN ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER')),
    CONSTRAINT ck_number_suspensions_provider_state CHECK (provider_state IN
        ('NOT_REQUIRED', 'PENDING', 'CONFIRMED', 'UNSUPPORTED', 'UNKNOWN', 'FAILED')),
    CONSTRAINT ck_number_suspensions_status CHECK (status IN ('SUSPENDED', 'RESTORING', 'RESTORED', 'ENDED')),
    CONSTRAINT ck_number_suspensions_restored CHECK ((status = 'RESTORED') = (restored_at IS NOT NULL)),
    CONSTRAINT ck_number_suspensions_restoring CHECK (status = 'SUSPENDED' OR restore_requested_at IS NOT NULL OR status = 'ENDED')
);
CREATE UNIQUE INDEX uq_number_suspensions_open ON number_suspensions (number_resource_id)
    WHERE status IN ('SUSPENDED', 'RESTORING');
CREATE INDEX ix_number_suspensions_scope ON number_suspensions (tenant_id, application_id, environment_id, suspended_at);
CREATE TRIGGER trg_number_suspensions_guard
    BEFORE UPDATE OR DELETE ON number_suspensions
    FOR EACH ROW EXECUTE FUNCTION number_guard_workflow(
        'provider_command_id', 'provider_state', 'status', 'revision', 'restore_requested_at', 'restore_actor_kind',
        'restore_actor_ref', 'restore_command_id', 'restore_checks', 'restore_blocked_reason', 'restored_at',
        'ended_reason', 'updated_at');

-- Q: release, quarantine, late activity ---------------------------------------------------
CREATE TABLE number_release_requests (
    id                   UUID         NOT NULL,
    number_resource_id   UUID         NOT NULL REFERENCES number_resources (id),
    assignment_id        UUID         NOT NULL REFERENCES dcn_assignments (id),
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    owner_kind           VARCHAR(16)  NOT NULL,
    owner_ref            UUID         NOT NULL,
    ownership_class      VARCHAR(24)  NOT NULL,
    reason_code          VARCHAR(64)  NOT NULL,
    policy               VARCHAR(24)  NOT NULL,
    requester_kind       VARCHAR(16)  NOT NULL,
    requester_ref        VARCHAR(200) NOT NULL,
    requested_at         TIMESTAMPTZ  NOT NULL,
    effective_at         TIMESTAMPTZ  NOT NULL,
    idempotency_key      VARCHAR(160) NOT NULL,
    status               VARCHAR(32)  NOT NULL,
    revision             BIGINT       NOT NULL DEFAULT 1,
    grants_stopped_at    TIMESTAMPTZ,
    routing_removed_at   TIMESTAMPTZ,
    routing_evidence     JSONB,
    detached_at          TIMESTAMPTZ,
    provider_command_id  UUID,
    provider_released_at TIMESTAMPTZ,
    quarantine_id        UUID,
    failure_reason       VARCHAR(64),
    updated_at           TIMESTAMPTZ  NOT NULL,
    correlation_id       VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_release_requests PRIMARY KEY (id),
    CONSTRAINT uq_number_release_requests_key UNIQUE (tenant_id, application_id, environment_id, idempotency_key),
    CONSTRAINT ck_number_release_requests_policy CHECK (policy IN ('POOL_RETURN', 'PROVIDER_RELINQUISH', 'PORTED_OUT')),
    CONSTRAINT ck_number_release_requests_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_release_requests_status CHECK (status IN ('REQUESTED', 'ROUTING_REMOVED',
        'PROVIDER_RELEASE_PENDING', 'PROVIDER_OUTCOME_UNKNOWN', 'QUARANTINED', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_number_release_requests_order CHECK (
        (status IN ('REQUESTED', 'CANCELLED', 'FAILED') OR (grants_stopped_at IS NOT NULL AND routing_removed_at IS NOT NULL))
        AND (status NOT IN ('PROVIDER_RELEASE_PENDING', 'PROVIDER_OUTCOME_UNKNOWN', 'QUARANTINED')
             OR (detached_at IS NOT NULL AND routing_evidence IS NOT NULL))
        AND (status <> 'QUARANTINED' OR quarantine_id IS NOT NULL)
        AND (policy = 'POOL_RETURN' OR status <> 'QUARANTINED' OR provider_released_at IS NOT NULL)),
    CONSTRAINT ck_number_release_requests_actor CHECK (requester_kind IN ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER'))
);
CREATE UNIQUE INDEX uq_number_release_requests_open ON number_release_requests (number_resource_id)
    WHERE status NOT IN ('QUARANTINED', 'FAILED', 'CANCELLED');
CREATE INDEX ix_number_release_requests_due ON number_release_requests (status, effective_at);
CREATE TRIGGER trg_number_release_requests_guard
    BEFORE UPDATE OR DELETE ON number_release_requests
    FOR EACH ROW EXECUTE FUNCTION number_guard_workflow(
        'status', 'revision', 'grants_stopped_at', 'routing_removed_at', 'routing_evidence', 'detached_at',
        'provider_command_id', 'provider_released_at', 'quarantine_id', 'failure_reason', 'updated_at');

CREATE TABLE number_quarantines (
    id                    UUID         NOT NULL,
    number_resource_id    UUID         NOT NULL REFERENCES number_resources (id),
    release_request_id    UUID         REFERENCES number_release_requests (id),
    previous_assignment_id UUID        REFERENCES dcn_assignments (id),
    disposition           VARCHAR(16)  NOT NULL,
    policy_ref            VARCHAR(64)  NOT NULL,
    cooldown_days         INTEGER      NOT NULL,
    started_at            TIMESTAMPTZ  NOT NULL,
    quarantine_until      TIMESTAMPTZ  NOT NULL,
    status                VARCHAR(16)  NOT NULL,
    revision              BIGINT       NOT NULL DEFAULT 1,
    review_required       BOOLEAN      NOT NULL DEFAULT FALSE,
    review_decision       VARCHAR(16),
    reviewed_by           VARCHAR(200),
    reviewed_at           TIMESTAMPTZ,
    extensions            INTEGER      NOT NULL DEFAULT 0,
    provider_confirmed_at TIMESTAMPTZ,
    provider_evidence     JSONB,
    completed_at          TIMESTAMPTZ,
    completion_reason     VARCHAR(64),
    updated_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_quarantines PRIMARY KEY (id),
    CONSTRAINT ck_number_quarantines_disposition CHECK (disposition IN ('POOL_RETURN', 'RELINQUISHED')),
    CONSTRAINT ck_number_quarantines_status CHECK (status IN ('ACTIVE', 'HELD_FOR_REVIEW', 'COMPLETED')),
    CONSTRAINT ck_number_quarantines_period CHECK (quarantine_until > started_at AND cooldown_days BETWEEN 1 AND 3650),
    CONSTRAINT ck_number_quarantines_review CHECK (review_decision IS NULL OR review_decision IN ('APPROVED', 'EXTENDED', 'HELD')),
    CONSTRAINT ck_number_quarantines_completed CHECK ((status = 'COMPLETED') = (completed_at IS NOT NULL)),
    CONSTRAINT ck_number_quarantines_confirmed CHECK (status <> 'COMPLETED' OR disposition = 'RELINQUISHED'
        OR provider_confirmed_at IS NOT NULL)
);
CREATE UNIQUE INDEX uq_number_quarantines_open ON number_quarantines (number_resource_id) WHERE status <> 'COMPLETED';
CREATE INDEX ix_number_quarantines_due ON number_quarantines (status, quarantine_until);
CREATE TRIGGER trg_number_quarantines_guard
    BEFORE UPDATE OR DELETE ON number_quarantines
    FOR EACH ROW EXECUTE FUNCTION number_guard_workflow(
        'quarantine_until', 'status', 'revision', 'review_required', 'review_decision', 'reviewed_by', 'reviewed_at',
        'extensions', 'provider_confirmed_at', 'provider_evidence', 'completed_at', 'completion_reason', 'updated_at');

CREATE TABLE number_late_activity (
    id                 UUID         NOT NULL,
    number_resource_id UUID         NOT NULL REFERENCES number_resources (id),
    quarantine_id      UUID         REFERENCES number_quarantines (id),
    family             VARCHAR(8)   NOT NULL,
    consumer           VARCHAR(64)  NOT NULL,
    lifecycle_state    VARCHAR(24)  NOT NULL,
    treatment          VARCHAR(16)  NOT NULL,
    observed_at        TIMESTAMPTZ  NOT NULL,
    correlation_id     VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_late_activity PRIMARY KEY (id),
    CONSTRAINT ck_number_late_activity_family CHECK (family IN ('VOICE', 'SMS')),
    CONSTRAINT ck_number_late_activity_treatment CHECK (treatment IN ('REFUSED')),
    CONSTRAINT ck_number_late_activity_consumer CHECK (consumer ~ '^cfmeet\.[a-z][a-z0-9-]{1,40}$')
);
CREATE INDEX ix_number_late_activity_number ON number_late_activity (number_resource_id, observed_at);
CREATE TRIGGER trg_number_late_activity_append_only
    BEFORE UPDATE OR DELETE ON number_late_activity
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

-- R: privileged history access ------------------------------------------------------------
CREATE TABLE number_history_access (
    id                 UUID         NOT NULL,
    number_resource_id UUID         NOT NULL REFERENCES number_resources (id),
    operator_ref       VARCHAR(200) NOT NULL,
    purpose            VARCHAR(32)  NOT NULL,
    case_ref           VARCHAR(64)  NOT NULL,
    access_kind        VARCHAR(32)  NOT NULL,
    outcome            VARCHAR(16)  NOT NULL,
    accessed_at        TIMESTAMPTZ  NOT NULL,
    correlation_id     VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_history_access PRIMARY KEY (id),
    CONSTRAINT ck_number_history_access_purpose CHECK (purpose IN
        ('ACCOUNT_RECOVERY', 'LEGAL_REQUEST', 'FRAUD_INVESTIGATION', 'CUSTOMER_SUPPORT', 'AUDIT')),
    CONSTRAINT ck_number_history_access_case CHECK (case_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{2,63}$'),
    CONSTRAINT ck_number_history_access_kind CHECK (access_kind IN ('OWNERSHIP_PERIODS', 'OWNERSHIP_VERIFICATION', 'LIFECYCLE_HISTORY')),
    CONSTRAINT ck_number_history_access_outcome CHECK (outcome IN ('READ', 'MATCH', 'NO_MATCH', 'AMBIGUOUS'))
);
CREATE INDEX ix_number_history_access_number ON number_history_access (number_resource_id, accessed_at);
CREATE TRIGGER trg_number_history_access_append_only
    BEFORE UPDATE OR DELETE ON number_history_access
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

-- S: usage and cost evidence ----------------------------------------------------------------
CREATE TABLE number_usage_records (
    id                 UUID         NOT NULL,
    source_key         VARCHAR(200) NOT NULL,
    number_resource_id UUID         NOT NULL REFERENCES number_resources (id),
    assignment_id      UUID         REFERENCES dcn_assignments (id),
    tenant_id          UUID,
    application_id     UUID,
    environment_id     UUID,
    usage_kind         VARCHAR(32)  NOT NULL,
    quantity           BIGINT       NOT NULL,
    unit               VARCHAR(16)  NOT NULL,
    provider_cost_minor BIGINT,
    currency           CHAR(3),
    cost_evidence      VARCHAR(16)  NOT NULL,
    period_start       TIMESTAMPTZ  NOT NULL,
    period_end         TIMESTAMPTZ  NOT NULL,
    reported_by        VARCHAR(64)  NOT NULL,
    status             VARCHAR(16)  NOT NULL,
    recorded_at        TIMESTAMPTZ  NOT NULL,
    finalized_at       TIMESTAMPTZ,
    CONSTRAINT pk_number_usage_records PRIMARY KEY (id),
    CONSTRAINT uq_number_usage_records_source UNIQUE (source_key),
    CONSTRAINT ck_number_usage_records_kind CHECK (usage_kind IN ('NUMBER_RENTAL', 'NUMBER_SETUP', 'NUMBER_RENEWAL',
        'VOICE_INBOUND_SECONDS', 'VOICE_OUTBOUND_SECONDS', 'SMS_INBOUND_SEGMENTS', 'SMS_OUTBOUND_SEGMENTS', 'PORTING')),
    CONSTRAINT ck_number_usage_records_unit CHECK (unit IN ('MONTH', 'EVENT', 'SECOND', 'SEGMENT')),
    CONSTRAINT ck_number_usage_records_quantity CHECK (quantity >= 0 AND (provider_cost_minor IS NULL OR provider_cost_minor >= 0)),
    CONSTRAINT ck_number_usage_records_currency CHECK ((provider_cost_minor IS NULL) = (currency IS NULL)
        AND (currency IS NULL OR currency ~ '^[A-Z]{3}$')),
    CONSTRAINT ck_number_usage_records_evidence CHECK (cost_evidence IN ('KNOWN', 'ESTIMATED', 'UNKNOWN')),
    CONSTRAINT ck_number_usage_records_period CHECK (period_end >= period_start),
    CONSTRAINT ck_number_usage_records_status CHECK (status IN ('PROVISIONAL', 'FINAL')),
    CONSTRAINT ck_number_usage_records_final CHECK ((status = 'FINAL') = (finalized_at IS NOT NULL)),
    CONSTRAINT ck_number_usage_records_scope CHECK ((tenant_id IS NULL) = (application_id IS NULL)
        AND (tenant_id IS NULL) = (environment_id IS NULL)),
    CONSTRAINT ck_number_usage_records_reporter CHECK (reported_by ~ '^[a-z][a-z0-9._-]{1,63}$')
);
CREATE INDEX ix_number_usage_records_scope ON number_usage_records (tenant_id, application_id, environment_id, period_start);
CREATE INDEX ix_number_usage_records_number ON number_usage_records (number_resource_id, period_start);
CREATE FUNCTION number_guard_usage() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'usage records are never deleted' USING ERRCODE = '42501';
    END IF;
    IF OLD.status = 'FINAL' THEN
        RAISE EXCEPTION 'a final usage record is immutable' USING ERRCODE = '42501';
    END IF;
    IF (to_jsonb(NEW) - 'status' - 'finalized_at' - 'provider_cost_minor' - 'currency' - 'cost_evidence')
       IS DISTINCT FROM (to_jsonb(OLD) - 'status' - 'finalized_at' - 'provider_cost_minor' - 'currency' - 'cost_evidence') THEN
        RAISE EXCEPTION 'a usage record changes only its cost reconciliation and finalization' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_usage_records_guard
    BEFORE UPDATE OR DELETE ON number_usage_records
    FOR EACH ROW EXECUTE FUNCTION number_guard_usage();

-- T: risk -----------------------------------------------------------------------------------
CREATE TABLE number_risk_signals (
    id                 UUID         NOT NULL,
    number_resource_id UUID         REFERENCES number_resources (id),
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    signal_kind        VARCHAR(32)  NOT NULL,
    magnitude          INTEGER      NOT NULL,
    source             VARCHAR(64)  NOT NULL,
    source_key         VARCHAR(200) NOT NULL,
    observed_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_risk_signals PRIMARY KEY (id),
    CONSTRAINT uq_number_risk_signals_source UNIQUE (source_key),
    CONSTRAINT ck_number_risk_signals_kind CHECK (signal_kind IN ('SMS_PUMPING', 'ABNORMAL_SMS_TRAFFIC', 'SPAM_CALLS',
        'NUISANCE_CALLS', 'RAPID_RELEASE', 'MASS_REGISTRATION', 'NUMBER_FARMING', 'COST_SPIKE', 'PREMIUM_DESTINATION')),
    CONSTRAINT ck_number_risk_signals_magnitude CHECK (magnitude BETWEEN 0 AND 100),
    CONSTRAINT ck_number_risk_signals_source CHECK (source ~ '^[a-z][a-z0-9._-]{1,63}$')
);
CREATE INDEX ix_number_risk_signals_number ON number_risk_signals (number_resource_id, observed_at);
CREATE INDEX ix_number_risk_signals_scope ON number_risk_signals (tenant_id, application_id, environment_id, observed_at);
CREATE TRIGGER trg_number_risk_signals_append_only
    BEFORE UPDATE OR DELETE ON number_risk_signals
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

CREATE TABLE number_risk_decisions (
    id                  UUID         NOT NULL,
    sequence            BIGSERIAL    NOT NULL,
    decision_kind       VARCHAR(24)  NOT NULL,
    number_resource_id  UUID         REFERENCES number_resources (id),
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    outcome             VARCHAR(16)  NOT NULL,
    score               INTEGER      NOT NULL,
    reasons             TEXT[]       NOT NULL,
    policy_version      VARCHAR(32)  NOT NULL,
    supersedes_id       UUID         REFERENCES number_risk_decisions (id),
    suspension_id       UUID         REFERENCES number_suspensions (id),
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_ref           VARCHAR(200) NOT NULL,
    statement_ref       UUID,
    decided_at          TIMESTAMPTZ  NOT NULL,
    correlation_id      VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_risk_decisions PRIMARY KEY (id),
    CONSTRAINT ck_number_risk_decisions_kind CHECK (decision_kind IN ('ALLOCATION', 'SCORE', 'SPEND', 'APPEAL_FILED',
        'APPEAL_UPHELD', 'APPEAL_REJECTED', 'RESTRICTION_LIFTED')),
    CONSTRAINT ck_number_risk_decisions_outcome CHECK (outcome IN ('ALLOW', 'REVIEW', 'DENY', 'RESTRICT', 'PENDING')),
    CONSTRAINT ck_number_risk_decisions_score CHECK (score BETWEEN 0 AND 100),
    CONSTRAINT ck_number_risk_decisions_reasons CHECK (reasons <@ ARRAY['ACTIVE_NUMBER_LIMIT', 'CLASS_LIMIT',
        'DAILY_VELOCITY', 'WEEKLY_VELOCITY', 'COUNTRY_RESTRICTED', 'MULTI_COUNTRY_REVIEW', 'MASS_REGISTRATION',
        'NUMBER_FARMING', 'RAPID_RELEASE', 'SMS_PUMPING', 'ABNORMAL_SMS_TRAFFIC', 'SPAM_CALLS', 'NUISANCE_CALLS',
        'SPEND_THRESHOLD', 'COST_SPIKE', 'PREMIUM_DESTINATION', 'APPEAL', 'OPERATOR_DECISION', 'WITHIN_POLICY']::TEXT[]),
    CONSTRAINT ck_number_risk_decisions_appeal CHECK (decision_kind NOT IN ('APPEAL_FILED', 'APPEAL_UPHELD',
        'APPEAL_REJECTED', 'RESTRICTION_LIFTED') OR supersedes_id IS NOT NULL),
    CONSTRAINT ck_number_risk_decisions_actor CHECK (actor_kind IN ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER'))
);
CREATE INDEX ix_number_risk_decisions_number ON number_risk_decisions (number_resource_id, sequence);
CREATE INDEX ix_number_risk_decisions_scope ON number_risk_decisions (tenant_id, application_id, environment_id, sequence);
CREATE TRIGGER trg_number_risk_decisions_append_only
    BEFORE UPDATE OR DELETE ON number_risk_decisions
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

-- U: portability ----------------------------------------------------------------------------
CREATE TABLE number_port_requests (
    id                  UUID         NOT NULL,
    direction           VARCHAR(8)   NOT NULL,
    number_resource_id  UUID         REFERENCES number_resources (id),
    assignment_id       UUID         REFERENCES dcn_assignments (id),
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    country_code        CHAR(2)      NOT NULL,
    number_digest       VARCHAR(64)  NOT NULL,
    ownership_class     VARCHAR(24)  NOT NULL,
    kyc_case_ref        UUID,
    evidence_refs       UUID[]       NOT NULL DEFAULT '{}',
    provider_id         VARCHAR(64),
    provider_command_id UUID,
    status              VARCHAR(32)  NOT NULL,
    revision            BIGINT       NOT NULL DEFAULT 1,
    status_reason       VARCHAR(64),
    idempotency_key     VARCHAR(160) NOT NULL,
    requested_by        VARCHAR(200) NOT NULL,
    requested_at        TIMESTAMPTZ  NOT NULL,
    completed_at        TIMESTAMPTZ,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_port_requests PRIMARY KEY (id),
    CONSTRAINT uq_number_port_requests_key UNIQUE (tenant_id, application_id, environment_id, idempotency_key),
    CONSTRAINT ck_number_port_requests_direction CHECK (direction IN ('IN', 'OUT')),
    CONSTRAINT ck_number_port_requests_status CHECK (status IN ('REQUESTED', 'ELIGIBILITY_CHECK', 'DOCUMENTS_REQUIRED',
        'SUBMITTED', 'IN_PROGRESS', 'COMPLETED', 'REJECTED', 'CANCELLED', 'UNKNOWN_PROVIDER_STATE')),
    CONSTRAINT ck_number_port_requests_out CHECK (direction = 'IN' OR (number_resource_id IS NOT NULL AND assignment_id IS NOT NULL)),
    CONSTRAINT ck_number_port_requests_digest CHECK (number_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_number_port_requests_completed CHECK ((status = 'COMPLETED') = (completed_at IS NOT NULL))
);
CREATE UNIQUE INDEX uq_number_port_requests_open ON number_port_requests (number_digest)
    WHERE status NOT IN ('COMPLETED', 'REJECTED', 'CANCELLED');
CREATE TRIGGER trg_number_port_requests_guard
    BEFORE UPDATE OR DELETE ON number_port_requests
    FOR EACH ROW EXECUTE FUNCTION number_guard_workflow(
        'number_resource_id', 'kyc_case_ref', 'evidence_refs', 'provider_id', 'provider_command_id', 'status',
        'revision', 'status_reason', 'completed_at', 'updated_at');

CREATE TABLE number_port_events (
    id               UUID         NOT NULL,
    sequence         BIGSERIAL    NOT NULL,
    port_request_id  UUID         NOT NULL REFERENCES number_port_requests (id),
    from_status      VARCHAR(32),
    to_status        VARCHAR(32)  NOT NULL,
    reason_code      VARCHAR(64)  NOT NULL,
    actor_kind       VARCHAR(16)  NOT NULL,
    actor_ref        VARCHAR(200) NOT NULL,
    evidence_ref     UUID,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_port_events PRIMARY KEY (id)
);
CREATE INDEX ix_number_port_events_request ON number_port_events (port_request_id, sequence);
CREATE TRIGGER trg_number_port_events_append_only
    BEFORE UPDATE OR DELETE ON number_port_events
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

-- W: operational controls -------------------------------------------------------------------
CREATE TABLE number_operational_controls (
    control_key   VARCHAR(40)  NOT NULL,
    engaged       BOOLEAN      NOT NULL,
    reason_code   VARCHAR(64)  NOT NULL,
    changed_by    VARCHAR(200) NOT NULL,
    changed_at    TIMESTAMPTZ  NOT NULL,
    revision      BIGINT       NOT NULL,
    CONSTRAINT pk_number_operational_controls PRIMARY KEY (control_key),
    CONSTRAINT ck_number_operational_controls_key CHECK (control_key IN ('DCN_INBOUND_ROUTING', 'DCN_CALLER_IDENTITY',
        'DCN_SMS_SENDER', 'NUMBER_PROVISIONING', 'NUMBER_PORTING', 'RELAY_LEASES'))
);
CREATE TRIGGER trg_number_operational_controls_no_delete
    BEFORE DELETE ON number_operational_controls
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TABLE number_control_events (
    id           UUID         NOT NULL,
    control_key  VARCHAR(40)  NOT NULL,
    engaged      BOOLEAN      NOT NULL,
    reason_code  VARCHAR(64)  NOT NULL,
    changed_by   VARCHAR(200) NOT NULL,
    changed_at   TIMESTAMPTZ  NOT NULL,
    revision     BIGINT       NOT NULL,
    CONSTRAINT pk_number_control_events PRIMARY KEY (id)
);
CREATE TRIGGER trg_number_control_events_append_only
    BEFORE UPDATE OR DELETE ON number_control_events
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();

-- Part P §§119–120: relay numbers and their leases -------------------------------------------
CREATE TABLE number_relay_pool_members (
    number_resource_id  UUID         NOT NULL REFERENCES number_resources (id),
    relay_inventory_ref UUID         NOT NULL,
    binding_ref         VARCHAR(128) NOT NULL,
    capabilities        TEXT[]       NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    imported_at         TIMESTAMPTZ  NOT NULL,
    imported_by         VARCHAR(64)  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_relay_pool_members PRIMARY KEY (number_resource_id),
    CONSTRAINT uq_number_relay_pool_members_ref UNIQUE (relay_inventory_ref),
    CONSTRAINT ck_number_relay_pool_members_status CHECK (status IN ('ACTIVE', 'RETIRED')),
    CONSTRAINT ck_number_relay_pool_members_caps CHECK (cardinality(capabilities) BETWEEN 1 AND 2
        AND capabilities <@ ARRAY['VOICE_INBOUND', 'VOICE_OUTBOUND']::TEXT[])
);
CREATE TRIGGER trg_number_relay_pool_members_guard
    BEFORE UPDATE OR DELETE ON number_relay_pool_members
    FOR EACH ROW EXECUTE FUNCTION number_guard_workflow('status', 'updated_at');

CREATE TABLE number_relay_leases (
    id                  UUID         NOT NULL,
    number_resource_id  UUID         NOT NULL REFERENCES number_relay_pool_members (number_resource_id),
    holder_service      VARCHAR(64)  NOT NULL,
    holder_ref          UUID         NOT NULL,
    generation          BIGINT       NOT NULL,
    idempotency_key     VARCHAR(160) NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    granted_at          TIMESTAMPTZ  NOT NULL,
    expires_at          TIMESTAMPTZ  NOT NULL,
    ended_at            TIMESTAMPTZ,
    end_reason          VARCHAR(32),
    correlation_id      VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_relay_leases PRIMARY KEY (id),
    CONSTRAINT uq_number_relay_leases_key UNIQUE (holder_service, idempotency_key),
    CONSTRAINT ck_number_relay_leases_status CHECK (status IN ('ACTIVE', 'RELEASED', 'EXPIRED', 'REVOKED')),
    CONSTRAINT ck_number_relay_leases_ended CHECK ((status = 'ACTIVE') = (ended_at IS NULL)
        AND (ended_at IS NULL) = (end_reason IS NULL)),
    CONSTRAINT ck_number_relay_leases_period CHECK (expires_at > granted_at),
    CONSTRAINT ck_number_relay_leases_holder CHECK (holder_service ~ '^cfmeet\.[a-z][a-z0-9-]{1,40}$'),
    CONSTRAINT ck_number_relay_leases_generation CHECK (generation >= 1)
);
CREATE UNIQUE INDEX uq_number_relay_leases_active ON number_relay_leases (number_resource_id) WHERE status = 'ACTIVE';
CREATE INDEX ix_number_relay_leases_expiry ON number_relay_leases (expires_at) WHERE status = 'ACTIVE';
CREATE TRIGGER trg_number_relay_leases_guard
    BEFORE UPDATE OR DELETE ON number_relay_leases
    FOR EACH ROW EXECUTE FUNCTION number_guard_workflow('status', 'ended_at', 'end_reason');
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_release_requests) OR EXISTS (SELECT 1 FROM number_quarantines)
       OR EXISTS (SELECT 1 FROM number_suspensions) OR EXISTS (SELECT 1 FROM number_relay_pool_members)
       OR EXISTS (SELECT 1 FROM number_port_requests) OR EXISTS (SELECT 1 FROM number_usage_records)
       OR EXISTS (SELECT 1 FROM number_risk_decisions) OR EXISTS (SELECT 1 FROM number_history_access) THEN
        RAISE EXCEPTION 'lifecycle stewardship evidence is recorded; it cannot be represented before Workstreams P-X';
    END IF;
END $$;
DROP TABLE IF EXISTS number_relay_leases;
DROP TABLE IF EXISTS number_relay_pool_members;
DROP TABLE IF EXISTS number_control_events;
DROP TABLE IF EXISTS number_operational_controls;
DROP TABLE IF EXISTS number_port_events;
DROP TABLE IF EXISTS number_port_requests;
DROP TABLE IF EXISTS number_risk_decisions;
DROP TABLE IF EXISTS number_risk_signals;
DROP TABLE IF EXISTS number_usage_records;
DROP TABLE IF EXISTS number_history_access;
DROP TABLE IF EXISTS number_late_activity;
DROP TABLE IF EXISTS number_quarantines;
DROP TABLE IF EXISTS number_release_requests;
DROP TABLE IF EXISTS number_suspensions;
DROP TRIGGER IF EXISTS trg_dcn_assignments_dcn_only ON dcn_assignments;
DROP TRIGGER IF EXISTS trg_number_reservations_dcn_only ON number_reservations;
DROP TRIGGER IF EXISTS trg_number_resources_service_class ON number_resources;
DROP FUNCTION IF EXISTS number_guard_usage();
DROP FUNCTION IF EXISTS number_guard_workflow();
DROP FUNCTION IF EXISTS number_guard_dcn_only();
DROP FUNCTION IF EXISTS number_guard_service_class();
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_resources WHERE service_class <> 'DCN') THEN
        RAISE EXCEPTION 'relay-class numbers exist; they cannot be represented before Part P';
    END IF;
END $$;
ALTER TABLE number_resources DROP CONSTRAINT ck_number_resources_service_class;
ALTER TABLE number_resources DROP COLUMN service_class;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM numbering_incidents WHERE category IN ('LIFECYCLE_SAFETY', 'QUARANTINE_CONFLICT',
                 'PROVIDER_COST_SPIKE', 'RISK_RESTRICTION')) THEN
        RAISE EXCEPTION 'lifecycle incidents are recorded; they cannot be represented before Workstreams P-X';
    END IF;
END $$;
ALTER TABLE numbering_incidents DROP CONSTRAINT ck_numbering_incidents_category;
ALTER TABLE numbering_incidents ADD CONSTRAINT ck_numbering_incidents_category CHECK (category IN ('PROVIDER_STATE_DRIFT', 'UNKNOWN_PROVIDER_OUTCOME',
        'CALLBACK_AUTHENTICATION_FAILED', 'UNMATCHED_PROVIDER_EVENT', 'PROVIDER_RELEASED_UNEXPECTEDLY',
        'RECONCILIATION_CONFLICT', 'PROVIDER_OUTAGE'));
ALTER TABLE numbering_outbox_events DROP CONSTRAINT ck_numbering_outbox_events_type;
ALTER TABLE numbering_outbox_events ADD CONSTRAINT ck_numbering_outbox_events_type
    CHECK (event_type ~ '^number(\.[a-z][a-z0-9-]*){2,}$' AND length(event_type) <= 80);
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
