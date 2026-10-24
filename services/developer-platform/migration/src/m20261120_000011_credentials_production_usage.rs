//! Part VI Chapters 10–13 (Sections 54–57).
//!
//! Section 54 — credentials, scopes and quotas (credentials themselves stay in Platform
//! Access: nothing here holds a credential, verifier or secret):
//! * `devplat_org_credential_policies`: an organization's credential lifetime / rotation
//!   overlap policy (applied before delegating issuance to Platform Access).
//! * `devplat_quota_restrictions`: service-account / credential API quota restrictions
//!   (they can only make the effective limits stricter).
//! * `devplat_entitlement_overrides`: explicit, audited operator overrides that let a
//!   project exceed its organization plan (the only way it can).
//! * `devplat_entitlement_snapshots`: versioned effective entitlement snapshots per
//!   environment for consuming services (append-only).
//!
//! Section 55 — production access and the unified usage ledger:
//! * `devplat_production_access_requests` / `_transitions`: the SANDBOX_READY →
//!   PRODUCTION_REQUESTED → UNDER_REVIEW → APPROVED / REJECTED → LIVE / SUSPENDED workflow
//!   with an immutable transition history.
//! * `devplat_usage_facts`: immutable normalized usage facts, unique on
//!   `(producer, source_fact_id)`, with a payload digest (conflicting replays refused).
//! * `devplat_usage_rollups_daily`: rebuildable daily aggregates.
//! * `devplat_usage_streams`: per-producer export watermarks (durable backlog recovery).
//! * `devplat_usage_rejections`: refused (conflicting / invalid) facts, for operators.
//! * `devplat_usage_reconciliations`: shadow reconciliation of producer totals.
//!
//! Section 57 — `devplat_support_profiles`: support / incident routing metadata (no
//! ticketing).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Section 55: an activated PRODUCTION environment is LIVE.
ALTER TABLE devplat_environment_profiles DROP CONSTRAINT ck_devplat_environment_profiles_pa;
ALTER TABLE devplat_environment_profiles ADD CONSTRAINT ck_devplat_environment_profiles_pa CHECK (production_access_state IN
    ('NOT_APPLICABLE', 'SANDBOX_READY', 'PRODUCTION_REQUESTED', 'UNDER_REVIEW', 'APPROVED', 'REJECTED', 'SUSPENDED', 'LIVE'));

CREATE TABLE devplat_org_credential_policies (
    tenant_id                    UUID         NOT NULL,
    max_lifetime_days            INTEGER      NOT NULL,
    max_rotation_overlap_minutes INTEGER      NOT NULL,
    updated_by                   VARCHAR(100) NOT NULL,
    updated_at                   TIMESTAMPTZ  NOT NULL,
    version                      BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_org_credential_policies PRIMARY KEY (tenant_id),
    CONSTRAINT fk_devplat_org_credential_policies_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT ck_devplat_org_credential_policies_lifetime CHECK (max_lifetime_days BETWEEN 1 AND 730),
    CONSTRAINT ck_devplat_org_credential_policies_overlap CHECK (max_rotation_overlap_minutes BETWEEN 0 AND 10080)
);

CREATE TABLE devplat_quota_restrictions (
    id                      UUID         NOT NULL,
    tenant_id               UUID         NOT NULL,
    application_id          UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    subject_kind            VARCHAR(16)  NOT NULL,
    subject_id              UUID         NOT NULL,
    api_requests_per_minute BIGINT,
    api_requests_per_day    BIGINT,
    updated_by              VARCHAR(100) NOT NULL,
    created_at              TIMESTAMPTZ  NOT NULL,
    updated_at              TIMESTAMPTZ  NOT NULL,
    version                 BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_quota_restrictions PRIMARY KEY (id),
    CONSTRAINT uq_devplat_quota_restrictions_subject UNIQUE (subject_kind, subject_id),
    CONSTRAINT ck_devplat_quota_restrictions_kind CHECK (subject_kind IN ('SERVICE_ACCOUNT', 'CREDENTIAL')),
    CONSTRAINT ck_devplat_quota_restrictions_values CHECK (
        (api_requests_per_minute IS NULL OR api_requests_per_minute BETWEEN 0 AND 1000000000)
        AND (api_requests_per_day IS NULL OR api_requests_per_day BETWEEN 0 AND 1000000000000))
);
CREATE INDEX ix_devplat_quota_restrictions_environment ON devplat_quota_restrictions (environment_id);

CREATE TABLE devplat_entitlement_overrides (
    id               UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID,
    entitlements     JSONB        NOT NULL,
    reason_code      VARCHAR(64)  NOT NULL,
    created_by       VARCHAR(100) NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    revoked_at       TIMESTAMPTZ,
    revoked_by       VARCHAR(100),
    CONSTRAINT pk_devplat_entitlement_overrides PRIMARY KEY (id),
    CONSTRAINT fk_devplat_entitlement_overrides_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT ck_devplat_entitlement_overrides_json CHECK (jsonb_typeof(entitlements) = 'object' AND octet_length(entitlements::text) <= 2048),
    CONSTRAINT ck_devplat_entitlement_overrides_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_entitlement_overrides_revoked CHECK ((revoked_at IS NULL) = (revoked_by IS NULL))
);
CREATE UNIQUE INDEX uq_devplat_entitlement_overrides_active
    ON devplat_entitlement_overrides (tenant_id, COALESCE(application_id, '00000000-0000-0000-0000-000000000000'::uuid))
    WHERE revoked_at IS NULL;

CREATE TABLE devplat_entitlement_snapshots (
    environment_id   UUID         NOT NULL,
    version          BIGINT       NOT NULL,
    snapshot         JSONB        NOT NULL,
    snapshot_digest  CHAR(64)     NOT NULL,
    computed_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_entitlement_snapshots PRIMARY KEY (environment_id, version),
    CONSTRAINT ck_devplat_entitlement_snapshots_version CHECK (version >= 1),
    CONSTRAINT ck_devplat_entitlement_snapshots_digest CHECK (snapshot_digest ~ '^[0-9a-f]{64}$')
);
CREATE TRIGGER tr_devplat_entitlement_snapshots_append_only BEFORE UPDATE OR DELETE ON devplat_entitlement_snapshots
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_production_access_requests (
    id                        UUID          NOT NULL,
    tenant_id                 UUID          NOT NULL,
    application_id            UUID          NOT NULL,
    state                     VARCHAR(24)   NOT NULL,
    requested_capabilities    TEXT[]        NOT NULL,
    approved_capabilities     TEXT[]        NOT NULL DEFAULT '{}',
    use_case_summary          VARCHAR(1000),
    requested_by              VARCHAR(100)  NOT NULL,
    reviewer_ref              VARCHAR(100),
    decision_reason_code      VARCHAR(64),
    decision_policy           VARCHAR(24),
    approval_expires_at       TIMESTAMPTZ,
    production_environment_id UUID,
    decided_at                TIMESTAMPTZ,
    activated_at              TIMESTAMPTZ,
    created_at                TIMESTAMPTZ   NOT NULL,
    updated_at                TIMESTAMPTZ   NOT NULL,
    version                   BIGINT        NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_production_access_requests PRIMARY KEY (id),
    CONSTRAINT fk_devplat_production_access_requests_project FOREIGN KEY (application_id) REFERENCES devplat_project_profiles (application_id),
    CONSTRAINT ck_devplat_production_access_requests_state CHECK (state IN
        ('PRODUCTION_REQUESTED', 'UNDER_REVIEW', 'APPROVED', 'REJECTED', 'CANCELLED', 'LIVE', 'SUSPENDED')),
    CONSTRAINT ck_devplat_production_access_requests_caps CHECK (cardinality(requested_capabilities) BETWEEN 1 AND 16),
    CONSTRAINT ck_devplat_production_access_requests_reason CHECK (decision_reason_code IS NULL OR decision_reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_production_access_requests_policy CHECK (decision_policy IS NULL OR decision_policy IN ('OPERATOR', 'AUTOMATIC')),
    CONSTRAINT ck_devplat_production_access_requests_live CHECK (state NOT IN ('LIVE', 'SUSPENDED') OR production_environment_id IS NOT NULL)
);
-- At most one open request per project.
CREATE UNIQUE INDEX uq_devplat_production_access_requests_open
    ON devplat_production_access_requests (application_id)
    WHERE state IN ('PRODUCTION_REQUESTED', 'UNDER_REVIEW', 'APPROVED', 'LIVE', 'SUSPENDED');
CREATE INDEX ix_devplat_production_access_requests_state ON devplat_production_access_requests (state, created_at);
CREATE INDEX ix_devplat_production_access_requests_tenant ON devplat_production_access_requests (tenant_id, created_at);
CREATE TRIGGER tr_devplat_production_access_requests_no_delete BEFORE DELETE ON devplat_production_access_requests
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

CREATE TABLE devplat_production_access_transitions (
    id           UUID         NOT NULL,
    request_id   UUID         NOT NULL,
    tenant_id    UUID         NOT NULL,
    from_state   VARCHAR(24),
    to_state     VARCHAR(24)  NOT NULL,
    actor_kind   VARCHAR(16)  NOT NULL,
    actor_ref    VARCHAR(100) NOT NULL,
    reason_code  VARCHAR(64),
    occurred_at  TIMESTAMPTZ  NOT NULL,
    seq          BIGSERIAL    NOT NULL,
    CONSTRAINT pk_devplat_production_access_transitions PRIMARY KEY (id),
    CONSTRAINT fk_devplat_production_access_transitions_request FOREIGN KEY (request_id) REFERENCES devplat_production_access_requests (id),
    CONSTRAINT ck_devplat_production_access_transitions_actor CHECK (actor_kind IN ('USER', 'OPERATOR', 'SERVICE', 'POLICY'))
);
CREATE INDEX ix_devplat_production_access_transitions_request ON devplat_production_access_transitions (request_id, seq);
CREATE TRIGGER tr_devplat_production_access_transitions_append_only BEFORE UPDATE OR DELETE ON devplat_production_access_transitions
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_usage_facts (
    id               UUID          NOT NULL,
    producer         VARCHAR(64)   NOT NULL,
    source_fact_id   VARCHAR(160)  NOT NULL,
    payload_digest   CHAR(64)      NOT NULL,
    usage_type       VARCHAR(40)   NOT NULL,
    unit             VARCHAR(16)   NOT NULL,
    quantity         BIGINT        NOT NULL,
    tenant_id        UUID          NOT NULL,
    application_id   UUID          NOT NULL,
    environment_id   UUID          NOT NULL,
    environment_kind VARCHAR(16)   NOT NULL,
    resource_ref     VARCHAR(160)  NOT NULL,
    occurred_at      TIMESTAMPTZ   NOT NULL,
    usage_day        DATE          NOT NULL,
    period_start     TIMESTAMPTZ,
    period_end       TIMESTAMPTZ,
    dimensions       JSONB         NOT NULL DEFAULT '{}'::jsonb,
    cost_center_ref  VARCHAR(64),
    ingested_at      TIMESTAMPTZ   NOT NULL,
    seq              BIGSERIAL     NOT NULL,
    CONSTRAINT pk_devplat_usage_facts PRIMARY KEY (id),
    CONSTRAINT uq_devplat_usage_facts_source UNIQUE (producer, source_fact_id),
    CONSTRAINT uq_devplat_usage_facts_seq UNIQUE (seq),
    CONSTRAINT ck_devplat_usage_facts_type CHECK (usage_type IN ('API_OPERATION', 'MEETING_PARTICIPANT_MINUTE',
        'CALL_MINUTE_APP', 'CALL_MINUTE_PUBLIC_NETWORK', 'NUMBER_ACTIVE', 'SMS_SEGMENT', 'RECORDING_MINUTE',
        'STORAGE_BYTE_TIME')),
    CONSTRAINT ck_devplat_usage_facts_unit CHECK (unit IN ('OPERATION', 'MILLISECOND', 'SEGMENT', 'BYTE_SECOND')),
    CONSTRAINT ck_devplat_usage_facts_quantity CHECK (quantity >= 0),
    CONSTRAINT ck_devplat_usage_facts_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_devplat_usage_facts_digest CHECK (payload_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_devplat_usage_facts_source_id CHECK (source_fact_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$'),
    CONSTRAINT ck_devplat_usage_facts_ref CHECK (resource_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,159}$'),
    CONSTRAINT ck_devplat_usage_facts_period CHECK (period_start IS NULL OR period_end IS NULL OR period_end >= period_start),
    CONSTRAINT ck_devplat_usage_facts_dimensions CHECK (jsonb_typeof(dimensions) = 'object' AND octet_length(dimensions::text) <= 1024)
);
CREATE INDEX ix_devplat_usage_facts_environment ON devplat_usage_facts (environment_id, usage_day, usage_type, seq);
CREATE INDEX ix_devplat_usage_facts_cost_center ON devplat_usage_facts (cost_center_ref, usage_day) WHERE cost_center_ref IS NOT NULL;
CREATE TRIGGER tr_devplat_usage_facts_append_only BEFORE UPDATE OR DELETE ON devplat_usage_facts
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_usage_facts_no_truncate BEFORE TRUNCATE ON devplat_usage_facts
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_usage_rollups_daily (
    environment_id   UUID          NOT NULL,
    usage_day        DATE          NOT NULL,
    usage_type       VARCHAR(40)   NOT NULL,
    unit             VARCHAR(16)   NOT NULL,
    tenant_id        UUID          NOT NULL,
    application_id   UUID          NOT NULL,
    environment_kind VARCHAR(16)   NOT NULL,
    cost_center_ref  VARCHAR(64),
    quantity         BIGINT        NOT NULL,
    fact_count       BIGINT        NOT NULL,
    updated_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_usage_rollups_daily PRIMARY KEY (environment_id, usage_day, usage_type),
    CONSTRAINT ck_devplat_usage_rollups_daily_counts CHECK (quantity >= 0 AND fact_count >= 0)
);
CREATE INDEX ix_devplat_usage_rollups_daily_tenant ON devplat_usage_rollups_daily (tenant_id, usage_day);

CREATE TABLE devplat_usage_streams (
    producer      VARCHAR(64)   NOT NULL,
    stream        VARCHAR(64)   NOT NULL,
    position_at   TIMESTAMPTZ,
    position_key  VARCHAR(160),
    facts_total   BIGINT        NOT NULL DEFAULT 0,
    updated_at    TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_usage_streams PRIMARY KEY (producer, stream),
    CONSTRAINT ck_devplat_usage_streams_name CHECK (stream ~ '^[a-z][a-z0-9_.-]{0,63}$')
);

CREATE TABLE devplat_usage_rejections (
    id              UUID          NOT NULL,
    producer        VARCHAR(64)   NOT NULL,
    source_fact_id  VARCHAR(160)  NOT NULL,
    reason_code     VARCHAR(64)   NOT NULL,
    received_at     TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_usage_rejections PRIMARY KEY (id)
);
CREATE INDEX ix_devplat_usage_rejections_producer ON devplat_usage_rejections (producer, received_at DESC);
CREATE TRIGGER tr_devplat_usage_rejections_append_only BEFORE UPDATE OR DELETE ON devplat_usage_rejections
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_usage_reconciliations (
    id                UUID          NOT NULL,
    producer          VARCHAR(64)   NOT NULL,
    environment_id    UUID          NOT NULL,
    usage_type        VARCHAR(40)   NOT NULL,
    window_start      TIMESTAMPTZ   NOT NULL,
    window_end        TIMESTAMPTZ   NOT NULL,
    producer_quantity BIGINT        NOT NULL,
    producer_count    BIGINT        NOT NULL,
    ledger_quantity   BIGINT        NOT NULL,
    ledger_count      BIGINT        NOT NULL,
    outcome           VARCHAR(16)   NOT NULL,
    recorded_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_usage_reconciliations PRIMARY KEY (id),
    CONSTRAINT ck_devplat_usage_reconciliations_outcome CHECK (outcome IN ('MATCH', 'MISMATCH')),
    CONSTRAINT ck_devplat_usage_reconciliations_window CHECK (window_end > window_start)
);
CREATE INDEX ix_devplat_usage_reconciliations_recent ON devplat_usage_reconciliations (recorded_at DESC);
CREATE TRIGGER tr_devplat_usage_reconciliations_append_only BEFORE UPDATE OR DELETE ON devplat_usage_reconciliations
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_support_profiles (
    tenant_id                   UUID         NOT NULL,
    support_tier                VARCHAR(16)  NOT NULL,
    support_reference           VARCHAR(64),
    technical_contact_member_id UUID,
    incident_contact_member_id  UUID,
    last_incident_ref           VARCHAR(64),
    last_incident_at            TIMESTAMPTZ,
    updated_by                  VARCHAR(100) NOT NULL,
    updated_at                  TIMESTAMPTZ  NOT NULL,
    version                     BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_support_profiles PRIMARY KEY (tenant_id),
    CONSTRAINT fk_devplat_support_profiles_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT fk_devplat_support_profiles_tech FOREIGN KEY (technical_contact_member_id) REFERENCES devplat_workspace_members (id),
    CONSTRAINT fk_devplat_support_profiles_incident FOREIGN KEY (incident_contact_member_id) REFERENCES devplat_workspace_members (id),
    CONSTRAINT ck_devplat_support_profiles_tier CHECK (support_tier IN ('COMMUNITY', 'STANDARD', 'PREMIUM', 'ENTERPRISE', 'INTERNAL')),
    CONSTRAINT ck_devplat_support_profiles_ref CHECK (support_reference IS NULL OR support_reference ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$'),
    CONSTRAINT ck_devplat_support_profiles_incident CHECK (
        (last_incident_ref IS NULL) = (last_incident_at IS NULL)
        AND (last_incident_ref IS NULL OR last_incident_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$'))
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_support_profiles;
DROP TABLE IF EXISTS devplat_usage_reconciliations;
DROP TABLE IF EXISTS devplat_usage_rejections;
DROP TABLE IF EXISTS devplat_usage_streams;
DROP TABLE IF EXISTS devplat_usage_rollups_daily;
DROP TABLE IF EXISTS devplat_usage_facts;
DROP TABLE IF EXISTS devplat_production_access_transitions;
DROP TABLE IF EXISTS devplat_production_access_requests;
DROP TABLE IF EXISTS devplat_entitlement_snapshots;
DROP TABLE IF EXISTS devplat_entitlement_overrides;
DROP TABLE IF EXISTS devplat_quota_restrictions;
DROP TABLE IF EXISTS devplat_org_credential_policies;
ALTER TABLE devplat_environment_profiles DROP CONSTRAINT ck_devplat_environment_profiles_pa;
ALTER TABLE devplat_environment_profiles ADD CONSTRAINT ck_devplat_environment_profiles_pa CHECK (production_access_state IN
    ('NOT_APPLICABLE', 'SANDBOX_READY', 'PRODUCTION_REQUESTED', 'UNDER_REVIEW', 'APPROVED', 'REJECTED', 'SUSPENDED'));
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM devplat_usage_facts) \
                  OR EXISTS (SELECT 1 FROM devplat_production_access_requests) THEN \
                 RAISE EXCEPTION 'usage facts or production-access history exist; refusing to drop them'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(DOWN).await?;
        Ok(())
    }
}
