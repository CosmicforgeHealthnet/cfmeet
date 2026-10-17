//! Workstream F: KYC requirements, cases, protected evidence, verification, decision
//! history, appeals and eligibility decisions.
//!
//! * `number_kyc_requirements` — versioned requirement sets by country / class / number
//!   type / ownership class / provider extension / tenant classification. Synthetic sets
//!   are explicitly non-production (`inventory_class = 'SYNTHETIC'`, `SYNTHETIC_TEST`).
//! * `number_kyc_cases` — one subject's case in a trusted scope, with the EXACT
//!   requirement snapshot it was opened against; the lifecycle is enforced here
//!   (`number_kyc_transition_allowed`, session flag, revision step, immutable scope /
//!   subject / snapshot, decision fields required by state).
//! * `number_kyc_evidence_refs` — references to encrypted objects in protected storage
//!   (never the content); `number_kyc_evidence_access` — append-only audit of every
//!   privileged read.
//! * `number_kyc_decisions` — the immutable decision history of a case (one row per case
//!   revision); `number_kyc_appeals` — appeals linked to the decision they contest;
//!   `number_kyc_verifications` — verifier attempts (unknown outcomes are explicit).
//! * `number_eligibility_decisions` — append-only, explainable eligibility outcomes.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261018_000004_kyc_eligibility"
    }
}

const UP: &str = r#"
CREATE FUNCTION number_kyc_transition_allowed(from_status TEXT, to_status TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT (from_status, to_status) IN (
        ('DRAFT', 'EVIDENCE_REQUIRED'), ('DRAFT', 'EXPIRED'),
        ('EVIDENCE_REQUIRED', 'SUBMITTED'), ('EVIDENCE_REQUIRED', 'EXPIRED'),
        ('SUBMITTED', 'UNDER_REVIEW'),
        ('UNDER_REVIEW', 'APPROVED'), ('UNDER_REVIEW', 'REJECTED'), ('UNDER_REVIEW', 'MORE_EVIDENCE_REQUIRED'),
        ('MORE_EVIDENCE_REQUIRED', 'SUBMITTED'), ('MORE_EVIDENCE_REQUIRED', 'EXPIRED'),
        ('APPROVED', 'EXPIRED'), ('APPROVED', 'REVOKED'), ('APPROVED', 'MORE_EVIDENCE_REQUIRED'),
        ('APPROVED', 'APPROVED'),
        ('REJECTED', 'UNDER_REVIEW'),
        ('REVOKED', 'UNDER_REVIEW')
    )
$$;

-- ---------------------------------------------------------------------------
-- Requirement catalog (versioned per selector)
-- ---------------------------------------------------------------------------
CREATE TABLE number_kyc_requirements (
    id                     UUID         NOT NULL,
    selector_key           VARCHAR(300) NOT NULL,
    scope_kind             VARCHAR(24)  NOT NULL,
    country_code           CHAR(2)      NOT NULL,
    inventory_class        VARCHAR(16)  NOT NULL,
    number_type            VARCHAR(16),
    ownership_class        VARCHAR(24),
    provider_id            VARCHAR(64),
    tenant_classification  VARCHAR(16),
    kyc_mandatory          BOOLEAN      NOT NULL,
    items                  JSONB        NOT NULL,
    approval_validity_days INTEGER      NOT NULL,
    revalidation_days      INTEGER,
    source                 VARCHAR(32)  NOT NULL,
    source_version         VARCHAR(64)  NOT NULL,
    version                INTEGER      NOT NULL,
    effective_from         TIMESTAMPTZ  NOT NULL,
    effective_to           TIMESTAMPTZ,
    status                 VARCHAR(16)  NOT NULL DEFAULT 'CURRENT',
    recorded_by            VARCHAR(200) NOT NULL,
    recorded_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_kyc_requirements PRIMARY KEY (id),
    CONSTRAINT uq_number_kyc_requirements_version UNIQUE (selector_key, version),
    CONSTRAINT ck_number_kyc_requirements_scope CHECK (scope_kind IN ('BASE', 'PROVIDER_EXTENSION')
        AND (scope_kind = 'PROVIDER_EXTENSION') = (provider_id IS NOT NULL)),
    CONSTRAINT ck_number_kyc_requirements_country CHECK (country_code ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_number_kyc_requirements_class CHECK (inventory_class IN ('REAL', 'SYNTHETIC')),
    CONSTRAINT ck_number_kyc_requirements_type CHECK (number_type IS NULL OR number_type IN
        ('GEOGRAPHIC', 'MOBILE', 'TOLL_FREE', 'NATIONAL')),
    CONSTRAINT ck_number_kyc_requirements_ownership CHECK (ownership_class IS NULL OR ownership_class IN
        ('PERSONAL', 'PROFESSIONAL', 'ORGANIZATION', 'EMPLOYEE_DIRECT', 'DEPARTMENT', 'CAMPAIGN', 'SHARED_TEAM')),
    CONSTRAINT ck_number_kyc_requirements_provider CHECK (provider_id IS NULL OR provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_kyc_requirements_tenant CHECK (tenant_classification IS NULL OR tenant_classification IN
        ('FIRST_PARTY', 'EXTERNAL')),
    CONSTRAINT ck_number_kyc_requirements_items CHECK (jsonb_typeof(items) = 'array' AND jsonb_array_length(items) <= 16
        AND (kyc_mandatory = (jsonb_array_length(items) > 0) OR scope_kind = 'PROVIDER_EXTENSION')),
    CONSTRAINT ck_number_kyc_requirements_validity CHECK (approval_validity_days BETWEEN 1 AND 3660
        AND (revalidation_days IS NULL OR revalidation_days BETWEEN 1 AND approval_validity_days)),
    CONSTRAINT ck_number_kyc_requirements_source CHECK (source IN
        ('REGULATOR_PUBLICATION', 'PROVIDER_DOCUMENTATION', 'OPERATOR_ATTESTATION', 'SYNTHETIC_TEST')
        AND (inventory_class = 'SYNTHETIC') = (source = 'SYNTHETIC_TEST')
        AND (inventory_class = 'REAL' OR provider_id IS NULL OR provider_id = 'synthetic')),
    CONSTRAINT ck_number_kyc_requirements_source_version CHECK (source_version ~ '^[A-Za-z0-9._:-]{1,64}$'),
    CONSTRAINT ck_number_kyc_requirements_period CHECK (effective_to IS NULL OR effective_to > effective_from),
    CONSTRAINT ck_number_kyc_requirements_status CHECK (status IN ('CURRENT', 'SUPERSEDED')),
    CONSTRAINT ck_number_kyc_requirements_recorded_by CHECK (recorded_by ~ '^[A-Za-z0-9._:@-]{1,200}$')
);
CREATE UNIQUE INDEX uq_number_kyc_requirements_current ON number_kyc_requirements (selector_key) WHERE status = 'CURRENT';
CREATE INDEX ix_number_kyc_requirements_lookup ON number_kyc_requirements (country_code, inventory_class, status);

CREATE FUNCTION number_guard_kyc_requirement() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'requirement history is never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'CURRENT' THEN
            RAISE EXCEPTION 'a requirement set is inserted CURRENT' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF OLD.status <> 'CURRENT' OR NEW.status <> 'SUPERSEDED'
       OR (to_jsonb(NEW) - 'status') IS DISTINCT FROM (to_jsonb(OLD) - 'status') THEN
        RAISE EXCEPTION 'a requirement set only ever becomes SUPERSEDED' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_kyc_requirements_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_kyc_requirements
    FOR EACH ROW EXECUTE FUNCTION number_guard_kyc_requirement();
CREATE TRIGGER trg_number_kyc_requirements_no_truncate
    BEFORE TRUNCATE ON number_kyc_requirements
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- KYC cases
-- ---------------------------------------------------------------------------
CREATE TABLE number_kyc_cases (
    id                   UUID         NOT NULL,
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    environment_kind     VARCHAR(16)  NOT NULL,
    created_by           UUID         NOT NULL,
    subject_kind         VARCHAR(16)  NOT NULL,
    subject_ref          UUID         NOT NULL,
    country_code         CHAR(2)      NOT NULL,
    inventory_class      VARCHAR(16)  NOT NULL,
    number_type          VARCHAR(16)  NOT NULL,
    ownership_class      VARCHAR(24)  NOT NULL,
    purpose_code         VARCHAR(64)  NOT NULL,
    provider_id          VARCHAR(64),
    requirement_set_ids  UUID[]       NOT NULL,
    requirement_snapshot JSONB        NOT NULL,
    status               VARCHAR(24)  NOT NULL,
    revision             BIGINT       NOT NULL DEFAULT 1,
    submitted_at         TIMESTAMPTZ,
    decided_at           TIMESTAMPTZ,
    decision_reason      VARCHAR(64),
    verifier_kind        VARCHAR(16),
    reviewer_kind        VARCHAR(16),
    reviewer_ref         VARCHAR(200),
    approved_at          TIMESTAMPTZ,
    expires_at           TIMESTAMPTZ,
    revalidate_at        TIMESTAMPTZ,
    revoked_at           TIMESTAMPTZ,
    correlation_id       VARCHAR(64)  NOT NULL,
    created_at           TIMESTAMPTZ  NOT NULL,
    updated_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_kyc_cases PRIMARY KEY (id),
    CONSTRAINT uq_number_kyc_cases_scope UNIQUE (id, tenant_id, application_id, environment_id),
    CONSTRAINT ck_number_kyc_cases_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_number_kyc_cases_class CHECK (inventory_class IN ('REAL', 'SYNTHETIC')
        AND (inventory_class = 'REAL') = (environment_kind = 'PRODUCTION')),
    CONSTRAINT ck_number_kyc_cases_subject CHECK (subject_kind IN ('INDIVIDUAL', 'ORGANIZATION')),
    CONSTRAINT ck_number_kyc_cases_type CHECK (number_type IN ('GEOGRAPHIC', 'MOBILE', 'TOLL_FREE', 'NATIONAL')),
    CONSTRAINT ck_number_kyc_cases_ownership CHECK (ownership_class IN
        ('PERSONAL', 'PROFESSIONAL', 'ORGANIZATION', 'EMPLOYEE_DIRECT', 'DEPARTMENT', 'CAMPAIGN', 'SHARED_TEAM')
        AND (ownership_class IN ('PERSONAL', 'PROFESSIONAL')) = (subject_kind = 'INDIVIDUAL')),
    CONSTRAINT ck_number_kyc_cases_purpose CHECK (purpose_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_kyc_cases_provider CHECK (provider_id IS NULL OR provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_kyc_cases_snapshot CHECK (jsonb_typeof(requirement_snapshot) = 'object'
        AND cardinality(requirement_set_ids) >= 1),
    CONSTRAINT ck_number_kyc_cases_status CHECK (status IN ('DRAFT', 'EVIDENCE_REQUIRED', 'SUBMITTED', 'UNDER_REVIEW',
        'MORE_EVIDENCE_REQUIRED', 'APPROVED', 'REJECTED', 'EXPIRED', 'REVOKED')),
    CONSTRAINT ck_number_kyc_cases_revision CHECK (revision >= 1),
    CONSTRAINT ck_number_kyc_cases_verifier CHECK (verifier_kind IS NULL OR verifier_kind IN ('DETERMINISTIC', 'MANUAL', 'SYSTEM')),
    CONSTRAINT ck_number_kyc_cases_reviewer CHECK (reviewer_kind IS NULL OR reviewer_kind IN
        ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER')),
    CONSTRAINT ck_number_kyc_cases_reason CHECK (decision_reason IS NULL OR decision_reason ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    -- An approval names who decided, when, with what, and until when.
    CONSTRAINT ck_number_kyc_cases_approved CHECK (status <> 'APPROVED' OR (approved_at IS NOT NULL AND expires_at IS NOT NULL
        AND expires_at > approved_at AND decided_at IS NOT NULL AND verifier_kind IS NOT NULL AND reviewer_ref IS NOT NULL)),
    CONSTRAINT ck_number_kyc_cases_rejected CHECK (status NOT IN ('REJECTED', 'REVOKED', 'MORE_EVIDENCE_REQUIRED')
        OR (decision_reason IS NOT NULL AND decided_at IS NOT NULL)),
    CONSTRAINT ck_number_kyc_cases_revoked CHECK ((status = 'REVOKED') <= (revoked_at IS NOT NULL)),
    CONSTRAINT ck_number_kyc_cases_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE INDEX ix_number_kyc_cases_subject ON number_kyc_cases (tenant_id, application_id, environment_id, subject_ref, created_at);
CREATE INDEX ix_number_kyc_cases_expiry ON number_kyc_cases (expires_at) WHERE status = 'APPROVED';
CREATE INDEX ix_number_kyc_cases_revalidation ON number_kyc_cases (revalidate_at) WHERE status = 'APPROVED';

CREATE FUNCTION number_guard_kyc_case() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'KYC cases are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.status <> 'DRAFT' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'a KYC case starts DRAFT at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['status', 'revision', 'submitted_at', 'decided_at', 'decision_reason', 'verifier_kind',
            'reviewer_kind', 'reviewer_ref', 'approved_at', 'expires_at', 'revalidate_at', 'revoked_at', 'updated_at'])
       IS DISTINCT FROM
       (to_jsonb(OLD) - ARRAY['status', 'revision', 'submitted_at', 'decided_at', 'decision_reason', 'verifier_kind',
            'reviewer_kind', 'reviewer_ref', 'approved_at', 'expires_at', 'revalidate_at', 'revoked_at', 'updated_at']) THEN
        RAISE EXCEPTION 'the scope, subject and requirement snapshot of a KYC case are immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'KYC case updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF current_setting('numbering.kyc', true) IS DISTINCT FROM 'transition' THEN
        RAISE EXCEPTION 'KYC cases change only through a domain transition' USING ERRCODE = '42501';
    END IF;
    IF NOT number_kyc_transition_allowed(OLD.status, NEW.status) THEN
        RAISE EXCEPTION 'KYC transition % -> % is not allowed', OLD.status, NEW.status USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_kyc_cases_guard
    BEFORE INSERT OR UPDATE OR DELETE ON number_kyc_cases
    FOR EACH ROW EXECUTE FUNCTION number_guard_kyc_case();
CREATE TRIGGER trg_number_kyc_cases_no_truncate
    BEFORE TRUNCATE ON number_kyc_cases
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Protected evidence references (never the content)
-- ---------------------------------------------------------------------------
CREATE TABLE number_kyc_evidence_refs (
    id               UUID         NOT NULL,
    case_id          UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    evidence_kind    VARCHAR(40)  NOT NULL,
    requirement_type VARCHAR(32)  NOT NULL,
    object_key       VARCHAR(200) NOT NULL,
    content_type     VARCHAR(64)  NOT NULL,
    size_bytes       BIGINT       NOT NULL,
    sha256           CHAR(64)     NOT NULL,
    sealed_kid       VARCHAR(32)  NOT NULL,
    status           VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    submitted_by     UUID         NOT NULL,
    submitted_at     TIMESTAMPTZ  NOT NULL,
    retention_until  TIMESTAMPTZ  NOT NULL,
    purged_at        TIMESTAMPTZ,
    CONSTRAINT pk_number_kyc_evidence_refs PRIMARY KEY (id),
    CONSTRAINT uq_number_kyc_evidence_refs_object UNIQUE (object_key),
    CONSTRAINT fk_number_kyc_evidence_refs_case FOREIGN KEY (case_id, tenant_id, application_id, environment_id)
        REFERENCES number_kyc_cases (id, tenant_id, application_id, environment_id),
    CONSTRAINT ck_number_kyc_evidence_refs_kind CHECK (evidence_kind IN ('IDENTITY_DOCUMENT', 'PASSPORT',
        'BUSINESS_REGISTRATION_CERTIFICATE', 'PROOF_OF_ADDRESS', 'LOCAL_PRESENCE_ATTESTATION', 'PURPOSE_STATEMENT',
        'BENEFICIAL_OWNERSHIP_DECLARATION')),
    CONSTRAINT ck_number_kyc_evidence_refs_requirement CHECK (requirement_type IN ('INDIVIDUAL_IDENTITY',
        'BUSINESS_REGISTRATION', 'ADDRESS', 'LOCAL_PRESENCE', 'PURPOSE_OF_USE', 'BENEFICIAL_OWNERSHIP')),
    CONSTRAINT ck_number_kyc_evidence_refs_object CHECK (object_key ~ '^kyc-evidence/[0-9a-f-]{36}/[0-9a-f-]{36}$'),
    CONSTRAINT ck_number_kyc_evidence_refs_content CHECK (content_type IN ('application/pdf', 'image/jpeg', 'image/png')),
    CONSTRAINT ck_number_kyc_evidence_refs_size CHECK (size_bytes BETWEEN 1 AND 5242880),
    CONSTRAINT ck_number_kyc_evidence_refs_sha CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_number_kyc_evidence_refs_kid CHECK (sealed_kid ~ '^[A-Za-z0-9_-]{1,32}$'),
    CONSTRAINT ck_number_kyc_evidence_refs_status CHECK (status IN ('ACTIVE', 'PURGED')
        AND (status = 'PURGED') = (purged_at IS NOT NULL)),
    CONSTRAINT ck_number_kyc_evidence_refs_retention CHECK (retention_until > submitted_at)
);
CREATE INDEX ix_number_kyc_evidence_refs_case ON number_kyc_evidence_refs (case_id, submitted_at);
CREATE INDEX ix_number_kyc_evidence_refs_retention ON number_kyc_evidence_refs (retention_until) WHERE status = 'ACTIVE';

CREATE FUNCTION number_guard_kyc_evidence() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'evidence references are never deleted (content is purged)' USING ERRCODE = '42501';
    END IF;
    IF OLD.status <> 'ACTIVE' OR NEW.status <> 'PURGED'
       OR (to_jsonb(NEW) - 'status' - 'purged_at') IS DISTINCT FROM (to_jsonb(OLD) - 'status' - 'purged_at') THEN
        RAISE EXCEPTION 'an evidence reference only ever becomes PURGED' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_kyc_evidence_refs_guard
    BEFORE UPDATE OR DELETE ON number_kyc_evidence_refs
    FOR EACH ROW EXECUTE FUNCTION number_guard_kyc_evidence();
CREATE TRIGGER trg_number_kyc_evidence_refs_no_truncate
    BEFORE TRUNCATE ON number_kyc_evidence_refs
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

CREATE TABLE number_kyc_evidence_access (
    id             UUID         NOT NULL,
    evidence_id    UUID         NOT NULL,
    case_id        UUID         NOT NULL,
    accessor_kind  VARCHAR(16)  NOT NULL,
    accessor_ref   VARCHAR(200) NOT NULL,
    purpose_code   VARCHAR(64)  NOT NULL,
    outcome        VARCHAR(16)  NOT NULL,
    correlation_id VARCHAR(64)  NOT NULL,
    accessed_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_kyc_evidence_access PRIMARY KEY (id),
    CONSTRAINT fk_number_kyc_evidence_access_evidence FOREIGN KEY (evidence_id) REFERENCES number_kyc_evidence_refs (id),
    CONSTRAINT ck_number_kyc_evidence_access_kind CHECK (accessor_kind IN ('OPERATOR', 'SERVICE', 'SYSTEM')),
    CONSTRAINT ck_number_kyc_evidence_access_ref CHECK (accessor_ref ~ '^[A-Za-z0-9._:@-]{1,200}$'),
    CONSTRAINT ck_number_kyc_evidence_access_purpose CHECK (purpose_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_kyc_evidence_access_outcome CHECK (outcome IN ('GRANTED', 'DENIED', 'PURGED')),
    CONSTRAINT ck_number_kyc_evidence_access_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE TRIGGER trg_number_kyc_evidence_access_append_only
    BEFORE UPDATE OR DELETE ON number_kyc_evidence_access
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_kyc_evidence_access_no_truncate
    BEFORE TRUNCATE ON number_kyc_evidence_access
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Decision history (immutable, one row per case revision)
-- ---------------------------------------------------------------------------
CREATE TABLE number_kyc_decisions (
    id             UUID         NOT NULL,
    sequence       BIGSERIAL    NOT NULL,
    case_id        UUID         NOT NULL,
    case_revision  BIGINT       NOT NULL,
    from_status    VARCHAR(24),
    to_status      VARCHAR(24)  NOT NULL,
    decision_kind  VARCHAR(24)  NOT NULL,
    verifier_kind  VARCHAR(16)  NOT NULL,
    actor_kind     VARCHAR(16)  NOT NULL,
    actor_ref      VARCHAR(200) NOT NULL,
    reason_code    VARCHAR(64)  NOT NULL,
    evidence_ids   UUID[]       NOT NULL DEFAULT '{}',
    detail         JSONB        NOT NULL DEFAULT '{}'::jsonb,
    correlation_id VARCHAR(64)  NOT NULL,
    decided_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_kyc_decisions PRIMARY KEY (id),
    CONSTRAINT uq_number_kyc_decisions_revision UNIQUE (case_id, case_revision),
    CONSTRAINT fk_number_kyc_decisions_case FOREIGN KEY (case_id) REFERENCES number_kyc_cases (id),
    CONSTRAINT ck_number_kyc_decisions_edge CHECK (from_status IS NULL OR number_kyc_transition_allowed(from_status, to_status)),
    CONSTRAINT ck_number_kyc_decisions_kind CHECK (decision_kind IN ('CREATED', 'EVIDENCE_COLLECTION', 'SUBMISSION',
        'REVIEW_STARTED', 'VERIFICATION', 'MANUAL_REVIEW', 'REVALIDATION', 'EXPIRY', 'REVOCATION', 'APPEAL_OPENED',
        'APPEAL_RESOLVED')),
    CONSTRAINT ck_number_kyc_decisions_verifier CHECK (verifier_kind IN ('DETERMINISTIC', 'MANUAL', 'SYSTEM')),
    CONSTRAINT ck_number_kyc_decisions_actor CHECK (actor_kind IN ('OPERATOR', 'SERVICE', 'SYSTEM', 'RECONCILIATION', 'CUSTOMER')),
    CONSTRAINT ck_number_kyc_decisions_actor_ref CHECK (actor_ref ~ '^[A-Za-z0-9._:@-]{1,200}$'),
    CONSTRAINT ck_number_kyc_decisions_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_number_kyc_decisions_detail CHECK (jsonb_typeof(detail) = 'object' AND length(detail::text) <= 2000),
    -- A manual decision is an operator's.
    CONSTRAINT ck_number_kyc_decisions_manual CHECK (decision_kind <> 'MANUAL_REVIEW' OR (verifier_kind = 'MANUAL' AND actor_kind = 'OPERATOR')),
    CONSTRAINT ck_number_kyc_decisions_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE TRIGGER trg_number_kyc_decisions_append_only
    BEFORE UPDATE OR DELETE ON number_kyc_decisions
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_kyc_decisions_no_truncate
    BEFORE TRUNCATE ON number_kyc_decisions
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Appeals
-- ---------------------------------------------------------------------------
CREATE TABLE number_kyc_appeals (
    id                     UUID         NOT NULL,
    case_id                UUID         NOT NULL,
    appealed_decision_id   UUID         NOT NULL,
    status                 VARCHAR(16)  NOT NULL,
    reason_code            VARCHAR(64)  NOT NULL,
    submitted_by           UUID         NOT NULL,
    submitted_at           TIMESTAMPTZ  NOT NULL,
    resolved_at            TIMESTAMPTZ,
    resolution_decision_id UUID,
    CONSTRAINT pk_number_kyc_appeals PRIMARY KEY (id),
    CONSTRAINT fk_number_kyc_appeals_case FOREIGN KEY (case_id) REFERENCES number_kyc_cases (id),
    CONSTRAINT fk_number_kyc_appeals_decision FOREIGN KEY (appealed_decision_id) REFERENCES number_kyc_decisions (id),
    CONSTRAINT fk_number_kyc_appeals_resolution FOREIGN KEY (resolution_decision_id) REFERENCES number_kyc_decisions (id),
    CONSTRAINT ck_number_kyc_appeals_status CHECK (status IN ('OPEN', 'UPHELD', 'OVERTURNED')
        AND (status = 'OPEN') = (resolved_at IS NULL AND resolution_decision_id IS NULL)),
    CONSTRAINT ck_number_kyc_appeals_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
CREATE UNIQUE INDEX uq_number_kyc_appeals_open ON number_kyc_appeals (case_id) WHERE status = 'OPEN';
CREATE FUNCTION number_guard_kyc_appeal() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'appeals are never deleted' USING ERRCODE = '42501';
    END IF;
    IF OLD.status <> 'OPEN'
       OR (to_jsonb(NEW) - 'status' - 'resolved_at' - 'resolution_decision_id')
          IS DISTINCT FROM (to_jsonb(OLD) - 'status' - 'resolved_at' - 'resolution_decision_id') THEN
        RAISE EXCEPTION 'an appeal is resolved once' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_number_kyc_appeals_guard
    BEFORE UPDATE OR DELETE ON number_kyc_appeals
    FOR EACH ROW EXECUTE FUNCTION number_guard_kyc_appeal();
CREATE TRIGGER trg_number_kyc_appeals_no_truncate
    BEFORE TRUNCATE ON number_kyc_appeals
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Verifier attempts (unknown outcomes are explicit; responses deduplicated)
-- ---------------------------------------------------------------------------
CREATE TABLE number_kyc_verifications (
    id            UUID         NOT NULL,
    case_id       UUID         NOT NULL,
    case_revision BIGINT       NOT NULL,
    purpose       VARCHAR(16)  NOT NULL,
    verifier_kind VARCHAR(16)  NOT NULL,
    request_ref   UUID         NOT NULL,
    status        VARCHAR(16)  NOT NULL,
    outcome       VARCHAR(32),
    response_ref  VARCHAR(128),
    started_at    TIMESTAMPTZ  NOT NULL,
    completed_at  TIMESTAMPTZ,
    CONSTRAINT pk_number_kyc_verifications PRIMARY KEY (id),
    CONSTRAINT uq_number_kyc_verifications_request UNIQUE (request_ref),
    CONSTRAINT fk_number_kyc_verifications_case FOREIGN KEY (case_id) REFERENCES number_kyc_cases (id),
    CONSTRAINT ck_number_kyc_verifications_purpose CHECK (purpose IN ('SUBMISSION', 'REVALIDATION')),
    CONSTRAINT ck_number_kyc_verifications_verifier CHECK (verifier_kind IN ('DETERMINISTIC', 'MANUAL')),
    CONSTRAINT ck_number_kyc_verifications_status CHECK (status IN ('PENDING', 'COMPLETED', 'UNKNOWN', 'SUPERSEDED')
        AND (status = 'COMPLETED') = (completed_at IS NOT NULL AND outcome IS NOT NULL)),
    CONSTRAINT ck_number_kyc_verifications_outcome CHECK (outcome IS NULL OR outcome IN ('APPROVED', 'REJECTED',
        'MORE_EVIDENCE_REQUIRED', 'MISSING_EVIDENCE', 'PENDING_REVIEW'))
);
CREATE UNIQUE INDEX uq_number_kyc_verifications_response ON number_kyc_verifications (case_id, response_ref)
    WHERE response_ref IS NOT NULL;
CREATE UNIQUE INDEX uq_number_kyc_verifications_open ON number_kyc_verifications (case_id)
    WHERE status IN ('PENDING', 'UNKNOWN');

-- ---------------------------------------------------------------------------
-- Eligibility decisions (append-only, explainable)
-- ---------------------------------------------------------------------------
CREATE TABLE number_eligibility_decisions (
    id                     UUID         NOT NULL,
    tenant_id              UUID         NOT NULL,
    application_id         UUID         NOT NULL,
    environment_id         UUID         NOT NULL,
    environment_kind       VARCHAR(16)  NOT NULL,
    service_account_id     UUID         NOT NULL,
    subject_kind           VARCHAR(16)  NOT NULL,
    subject_ref            UUID         NOT NULL,
    country_code           CHAR(2)      NOT NULL,
    inventory_class        VARCHAR(16)  NOT NULL,
    number_type            VARCHAR(16)  NOT NULL,
    ownership_class        VARCHAR(24)  NOT NULL,
    number_resource_id     UUID,
    requested_capabilities TEXT[]       NOT NULL,
    outcome                VARCHAR(32)  NOT NULL,
    reasons                JSONB        NOT NULL,
    kyc_case_id            UUID,
    kyc_case_revision      BIGINT,
    requirement_set_ids    UUID[]       NOT NULL DEFAULT '{}',
    catalog_revision       BIGINT       NOT NULL,
    policy_version         VARCHAR(32)  NOT NULL,
    evaluated_at           TIMESTAMPTZ  NOT NULL,
    valid_until            TIMESTAMPTZ  NOT NULL,
    correlation_id         VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_number_eligibility_decisions PRIMARY KEY (id),
    CONSTRAINT fk_number_eligibility_decisions_resource FOREIGN KEY (number_resource_id) REFERENCES number_resources (id),
    CONSTRAINT fk_number_eligibility_decisions_case FOREIGN KEY (kyc_case_id, tenant_id, application_id, environment_id)
        REFERENCES number_kyc_cases (id, tenant_id, application_id, environment_id),
    CONSTRAINT ck_number_eligibility_decisions_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_number_eligibility_decisions_class CHECK (inventory_class IN ('REAL', 'SYNTHETIC')),
    CONSTRAINT ck_number_eligibility_decisions_outcome CHECK (outcome IN ('ELIGIBLE', 'KYC_REQUIRED',
        'ADDITIONAL_EVIDENCE_REQUIRED', 'PENDING_REVIEW', 'DENIED', 'UNSUPPORTED_CAPABILITY', 'LIMIT_EXCEEDED',
        'RISK_RESTRICTED')),
    CONSTRAINT ck_number_eligibility_decisions_reasons CHECK (jsonb_typeof(reasons) = 'array'
        AND (outcome = 'ELIGIBLE') = (jsonb_array_length(reasons) = 0)),
    CONSTRAINT ck_number_eligibility_decisions_valid CHECK (valid_until > evaluated_at),
    CONSTRAINT ck_number_eligibility_decisions_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$')
);
CREATE INDEX ix_number_eligibility_decisions_scope ON number_eligibility_decisions
    (tenant_id, application_id, environment_id, evaluated_at);
CREATE TRIGGER trg_number_eligibility_decisions_append_only
    BEFORE UPDATE OR DELETE ON number_eligibility_decisions
    FOR EACH ROW EXECUTE FUNCTION numbering_forbid_mutation();
CREATE TRIGGER trg_number_eligibility_decisions_no_truncate
    BEFORE TRUNCATE ON number_eligibility_decisions
    FOR EACH STATEMENT EXECUTE FUNCTION numbering_forbid_mutation();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_kyc_evidence_refs WHERE status = 'ACTIVE') THEN
        RAISE EXCEPTION 'active KYC evidence exists; purge it through retention before rolling back';
    END IF;
END $$;
DROP TABLE IF EXISTS number_eligibility_decisions;
DROP TABLE IF EXISTS number_kyc_verifications;
DROP TABLE IF EXISTS number_kyc_appeals;
DROP FUNCTION IF EXISTS number_guard_kyc_appeal();
DROP TABLE IF EXISTS number_kyc_decisions;
DROP TABLE IF EXISTS number_kyc_evidence_access;
DROP TABLE IF EXISTS number_kyc_evidence_refs;
DROP FUNCTION IF EXISTS number_guard_kyc_evidence();
DROP TABLE IF EXISTS number_kyc_cases;
DROP FUNCTION IF EXISTS number_guard_kyc_case();
DROP TABLE IF EXISTS number_kyc_requirements;
DROP FUNCTION IF EXISTS number_guard_kyc_requirement();
DROP FUNCTION IF EXISTS number_kyc_transition_allowed(TEXT, TEXT);
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
