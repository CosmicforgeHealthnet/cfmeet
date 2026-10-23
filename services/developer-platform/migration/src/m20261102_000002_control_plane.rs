//! Section 50 — the multi-tenant developer control plane.
//!
//! Every table is keyed to Platform Access canonical ids (tenant = organization,
//! application = project, environment = environment). Platform Access stays the identity
//! authority: no slug, verifier, credential, scope grant or principal is stored here. The
//! records below are Developer Platform metadata only:
//!
//! * `devplat_organizations` — public Organization profile + Part VI lifecycle state of a
//!   Platform Access tenant (one row per tenant).
//! * `devplat_workspace_members` — HUMAN developer workspace members and their Part VI
//!   role (OWNER / ADMIN / DEVELOPER / ANALYST / BILLING). Not Platform Access principals,
//!   not Meeting Service organization members.
//! * `devplat_invitations` — invitation lifecycle; the invitation token is stored as a
//!   SHA-256 digest only.
//! * `devplat_project_profiles` / `devplat_environment_profiles` — customer-facing
//!   metadata keyed one-to-one to application_id / environment_id.
//! * `devplat_commercial_plans`, `devplat_plan_assignments`, `devplat_billing_accounts` —
//!   versioned plans, effective-dated assignments and billing-account references. No
//!   price, card, charge, tax or balance column exists.
//! * `devplat_activity_events` — immutable tenant-scoped activity history (append-only by
//!   trigger; secret-looking summary keys refused).
//! * `devplat_idempotency_records` — replay protection of control-plane mutations.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_organizations (
    tenant_id           UUID         NOT NULL,
    display_name        VARCHAR(120) NOT NULL,
    state               VARCHAR(16)  NOT NULL,
    state_reason        VARCHAR(64),
    support_email       VARCHAR(254),
    technical_contact_member_id UUID,
    created_by          VARCHAR(100) NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    closed_at           TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_organizations PRIMARY KEY (tenant_id),
    CONSTRAINT ck_devplat_organizations_state CHECK (state IN ('INVITED', 'ACTIVE', 'RESTRICTED', 'SUSPENDED', 'CLOSED')),
    CONSTRAINT ck_devplat_organizations_name CHECK (char_length(btrim(display_name)) BETWEEN 1 AND 120),
    CONSTRAINT ck_devplat_organizations_reason CHECK (state_reason IS NULL OR state_reason ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_devplat_organizations_closed CHECK ((state = 'CLOSED') = (closed_at IS NOT NULL)),
    CONSTRAINT ck_devplat_organizations_version CHECK (version >= 1)
);

-- CLOSED is terminal.
CREATE FUNCTION devplat_organizations_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state = 'CLOSED' AND NEW.state <> 'CLOSED' THEN
        RAISE EXCEPTION 'devplat_organizations: CLOSED is terminal' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.tenant_id <> OLD.tenant_id OR NEW.created_at <> OLD.created_at OR NEW.created_by <> OLD.created_by THEN
        RAISE EXCEPTION 'devplat_organizations: identity columns are immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_organizations_guard BEFORE UPDATE ON devplat_organizations
    FOR EACH ROW EXECUTE FUNCTION devplat_organizations_guard();

CREATE TABLE devplat_workspace_members (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    user_id             UUID         NOT NULL,
    role                VARCHAR(16)  NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    joined_via          VARCHAR(16)  NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    removed_at          TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_workspace_members PRIMARY KEY (id),
    CONSTRAINT fk_devplat_workspace_members_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT ck_devplat_workspace_members_role CHECK (role IN ('OWNER', 'ADMIN', 'DEVELOPER', 'ANALYST', 'BILLING')),
    CONSTRAINT ck_devplat_workspace_members_status CHECK (status IN ('ACTIVE', 'REMOVED')),
    CONSTRAINT ck_devplat_workspace_members_via CHECK (joined_via IN ('CREATOR', 'INVITATION', 'OPERATOR')),
    CONSTRAINT ck_devplat_workspace_members_removed CHECK ((status = 'REMOVED') = (removed_at IS NOT NULL))
);
-- One ACTIVE membership per human per organization.
CREATE UNIQUE INDEX uq_devplat_workspace_members_active
    ON devplat_workspace_members (tenant_id, user_id) WHERE status = 'ACTIVE';
CREATE INDEX ix_devplat_workspace_members_user ON devplat_workspace_members (user_id) WHERE status = 'ACTIVE';

-- REMOVED is terminal; tenant and user never change.
CREATE FUNCTION devplat_workspace_members_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'REMOVED' THEN
        RAISE EXCEPTION 'devplat_workspace_members: REMOVED is terminal' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.tenant_id <> OLD.tenant_id OR NEW.user_id <> OLD.user_id OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'devplat_workspace_members: identity columns are immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_workspace_members_guard BEFORE UPDATE ON devplat_workspace_members
    FOR EACH ROW EXECUTE FUNCTION devplat_workspace_members_guard();

CREATE TABLE devplat_invitations (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    invitee_email       VARCHAR(254) NOT NULL,
    role                VARCHAR(16)  NOT NULL,
    token_digest        CHAR(64)     NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    invited_by          VARCHAR(100) NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    expires_at          TIMESTAMPTZ  NOT NULL,
    decided_at          TIMESTAMPTZ,
    accepted_by_user_id UUID,
    revoked_by_user_id  UUID,
    CONSTRAINT pk_devplat_invitations PRIMARY KEY (id),
    CONSTRAINT fk_devplat_invitations_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT uq_devplat_invitations_token UNIQUE (token_digest),
    CONSTRAINT ck_devplat_invitations_role CHECK (role IN ('OWNER', 'ADMIN', 'DEVELOPER', 'ANALYST', 'BILLING')),
    CONSTRAINT ck_devplat_invitations_status CHECK (status IN ('PENDING', 'ACCEPTED', 'REVOKED', 'EXPIRED')),
    CONSTRAINT ck_devplat_invitations_email CHECK (invitee_email = lower(invitee_email) AND invitee_email ~ '^[^@\s]+@[^@\s]+$'),
    CONSTRAINT ck_devplat_invitations_digest CHECK (token_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_devplat_invitations_expiry CHECK (expires_at > created_at),
    CONSTRAINT ck_devplat_invitations_decided CHECK ((status = 'PENDING') = (decided_at IS NULL)),
    CONSTRAINT ck_devplat_invitations_accepted CHECK ((status = 'ACCEPTED') = (accepted_by_user_id IS NOT NULL))
);
-- At most one PENDING invitation per address per organization.
CREATE UNIQUE INDEX uq_devplat_invitations_pending
    ON devplat_invitations (tenant_id, invitee_email) WHERE status = 'PENDING';

CREATE FUNCTION devplat_invitations_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status <> 'PENDING' THEN
        RAISE EXCEPTION 'devplat_invitations: a decided invitation is immutable' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.tenant_id <> OLD.tenant_id OR NEW.token_digest <> OLD.token_digest OR NEW.role <> OLD.role
       OR NEW.invitee_email <> OLD.invitee_email OR NEW.expires_at <> OLD.expires_at THEN
        RAISE EXCEPTION 'devplat_invitations: invitation terms are immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_invitations_guard BEFORE UPDATE ON devplat_invitations
    FOR EACH ROW EXECUTE FUNCTION devplat_invitations_guard();

CREATE TABLE devplat_project_profiles (
    application_id      UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    display_name        VARCHAR(120) NOT NULL,
    description         VARCHAR(1000),
    support_email       VARCHAR(254),
    labels              JSONB        NOT NULL DEFAULT '{}'::jsonb,
    status              VARCHAR(16)  NOT NULL,
    status_reason       VARCHAR(64),
    created_by          VARCHAR(100) NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    archived_at         TIMESTAMPTZ,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_project_profiles PRIMARY KEY (application_id),
    CONSTRAINT fk_devplat_project_profiles_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT ck_devplat_project_profiles_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'ARCHIVED')),
    CONSTRAINT ck_devplat_project_profiles_name CHECK (char_length(btrim(display_name)) BETWEEN 1 AND 120),
    CONSTRAINT ck_devplat_project_profiles_labels CHECK (jsonb_typeof(labels) = 'object' AND octet_length(labels::text) <= 4096),
    CONSTRAINT ck_devplat_project_profiles_archived CHECK ((status = 'ARCHIVED') = (archived_at IS NOT NULL))
);
CREATE INDEX ix_devplat_project_profiles_tenant ON devplat_project_profiles (tenant_id, created_at, application_id);

CREATE FUNCTION devplat_project_profiles_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'ARCHIVED' AND NEW.status <> 'ARCHIVED' THEN
        RAISE EXCEPTION 'devplat_project_profiles: ARCHIVED is terminal' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.application_id <> OLD.application_id OR NEW.tenant_id <> OLD.tenant_id OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'devplat_project_profiles: canonical references are immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_project_profiles_guard BEFORE UPDATE ON devplat_project_profiles
    FOR EACH ROW EXECUTE FUNCTION devplat_project_profiles_guard();
-- Projects are never deleted (archive only): domain data owned by other services is untouched.
CREATE FUNCTION devplat_no_delete() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '%: rows are never deleted', TG_TABLE_NAME USING ERRCODE = 'check_violation';
END $$;
CREATE TRIGGER tr_devplat_project_profiles_no_delete BEFORE DELETE ON devplat_project_profiles
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();
CREATE TRIGGER tr_devplat_organizations_no_delete BEFORE DELETE ON devplat_organizations
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

CREATE TABLE devplat_environment_profiles (
    environment_id      UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    kind                VARCHAR(16)  NOT NULL,
    display_name        VARCHAR(120) NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    production_access_state VARCHAR(24) NOT NULL,
    created_by          VARCHAR(100) NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_environment_profiles PRIMARY KEY (environment_id),
    CONSTRAINT fk_devplat_environment_profiles_project FOREIGN KEY (application_id) REFERENCES devplat_project_profiles (application_id),
    CONSTRAINT ck_devplat_environment_profiles_kind CHECK (kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_devplat_environment_profiles_status CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    CONSTRAINT ck_devplat_environment_profiles_pa CHECK (production_access_state IN
        ('NOT_APPLICABLE', 'SANDBOX_READY', 'PRODUCTION_REQUESTED', 'UNDER_REVIEW', 'APPROVED', 'REJECTED', 'SUSPENDED')),
    -- Section 55 (production approval) owns production environments: none is created here.
    CONSTRAINT ck_devplat_environment_profiles_nonprod CHECK (kind <> 'PRODUCTION' OR production_access_state <> 'NOT_APPLICABLE'),
    CONSTRAINT ck_devplat_environment_profiles_name CHECK (char_length(btrim(display_name)) BETWEEN 1 AND 120)
);
CREATE INDEX ix_devplat_environment_profiles_project ON devplat_environment_profiles (application_id, created_at);

CREATE FUNCTION devplat_environment_profiles_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.environment_id <> OLD.environment_id OR NEW.application_id <> OLD.application_id
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.kind <> OLD.kind OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'devplat_environment_profiles: canonical references are immutable' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_environment_profiles_guard BEFORE UPDATE ON devplat_environment_profiles
    FOR EACH ROW EXECUTE FUNCTION devplat_environment_profiles_guard();
CREATE TRIGGER tr_devplat_environment_profiles_no_delete BEFORE DELETE ON devplat_environment_profiles
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

-- Versioned commercial plans: entitlement profile only, never a price.
CREATE TABLE devplat_commercial_plans (
    id                  UUID         NOT NULL,
    code                VARCHAR(64)  NOT NULL,
    version             INTEGER      NOT NULL,
    display_name        VARCHAR(120) NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    entitlement_profile_ref VARCHAR(64) NOT NULL,
    entitlements        JSONB        NOT NULL,
    effective_from      TIMESTAMPTZ  NOT NULL,
    effective_until     TIMESTAMPTZ,
    created_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_commercial_plans PRIMARY KEY (id),
    CONSTRAINT uq_devplat_commercial_plans_version UNIQUE (code, version),
    CONSTRAINT ck_devplat_commercial_plans_code CHECK (code ~ '^[a-z][a-z0-9_-]{1,63}$'),
    CONSTRAINT ck_devplat_commercial_plans_version CHECK (version >= 1),
    CONSTRAINT ck_devplat_commercial_plans_status CHECK (status IN ('ACTIVE', 'RETIRED')),
    CONSTRAINT ck_devplat_commercial_plans_profile CHECK (entitlement_profile_ref ~ '^[a-z][a-z0-9._-]{1,63}$'),
    CONSTRAINT ck_devplat_commercial_plans_entitlements CHECK (jsonb_typeof(entitlements) = 'object'),
    CONSTRAINT ck_devplat_commercial_plans_window CHECK (effective_until IS NULL OR effective_until > effective_from)
);
-- A plan version's terms are immutable; only ACTIVE -> RETIRED (and closing its window) is allowed.
CREATE FUNCTION devplat_commercial_plans_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code <> OLD.code OR NEW.version <> OLD.version OR NEW.entitlements <> OLD.entitlements
       OR NEW.entitlement_profile_ref <> OLD.entitlement_profile_ref OR NEW.effective_from <> OLD.effective_from
       OR NEW.display_name <> OLD.display_name OR (OLD.status = 'RETIRED' AND NEW.status <> 'RETIRED') THEN
        RAISE EXCEPTION 'devplat_commercial_plans: plan versions are immutable (publish a new version)' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_commercial_plans_guard BEFORE UPDATE ON devplat_commercial_plans
    FOR EACH ROW EXECUTE FUNCTION devplat_commercial_plans_guard();
CREATE TRIGGER tr_devplat_commercial_plans_no_delete BEFORE DELETE ON devplat_commercial_plans
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

-- Effective-dated plan assignment history (tenant-wide, or a stricter project assignment).
CREATE TABLE devplat_plan_assignments (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID,
    plan_id             UUID         NOT NULL,
    source              VARCHAR(16)  NOT NULL,
    effective_from      TIMESTAMPTZ  NOT NULL,
    effective_until     TIMESTAMPTZ,
    superseded_by       UUID,
    assigned_by         VARCHAR(100) NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_plan_assignments PRIMARY KEY (id),
    CONSTRAINT fk_devplat_plan_assignments_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT fk_devplat_plan_assignments_plan FOREIGN KEY (plan_id) REFERENCES devplat_commercial_plans (id),
    CONSTRAINT fk_devplat_plan_assignments_project FOREIGN KEY (application_id) REFERENCES devplat_project_profiles (application_id),
    CONSTRAINT ck_devplat_plan_assignments_source CHECK (source IN ('INTERNAL', 'CONTRACT', 'SELF_SERVICE')),
    CONSTRAINT ck_devplat_plan_assignments_window CHECK (effective_until IS NULL OR effective_until > effective_from)
);
-- At most one open assignment per tenant (tenant level) and per project.
CREATE UNIQUE INDEX uq_devplat_plan_assignments_open_tenant
    ON devplat_plan_assignments (tenant_id) WHERE application_id IS NULL AND superseded_by IS NULL;
CREATE UNIQUE INDEX uq_devplat_plan_assignments_open_project
    ON devplat_plan_assignments (application_id) WHERE application_id IS NOT NULL AND superseded_by IS NULL;
-- History is immutable except closing it exactly once (supersession).
CREATE FUNCTION devplat_plan_assignments_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.superseded_by IS NOT NULL OR NEW.superseded_by IS NULL
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.plan_id <> OLD.plan_id OR NEW.effective_from <> OLD.effective_from OR NEW.source <> OLD.source THEN
        RAISE EXCEPTION 'devplat_plan_assignments: history is immutable (supersede once)' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_plan_assignments_guard BEFORE UPDATE ON devplat_plan_assignments
    FOR EACH ROW EXECUTE FUNCTION devplat_plan_assignments_guard();
CREATE TRIGGER tr_devplat_plan_assignments_no_delete BEFORE DELETE ON devplat_plan_assignments
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

-- Billing-account REFERENCE metadata: no card, charge, tax or balance data.
CREATE TABLE devplat_billing_accounts (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    classification      VARCHAR(24)  NOT NULL,
    status              VARCHAR(16)  NOT NULL,
    billing_contact_member_id UUID,
    currency            CHAR(3),
    country             CHAR(2),
    external_billing_ref VARCHAR(128),
    cost_center_ref     VARCHAR(64),
    created_at          TIMESTAMPTZ  NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    version             BIGINT       NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_billing_accounts PRIMARY KEY (id),
    CONSTRAINT uq_devplat_billing_accounts_tenant UNIQUE (tenant_id),
    CONSTRAINT fk_devplat_billing_accounts_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT fk_devplat_billing_accounts_contact FOREIGN KEY (billing_contact_member_id) REFERENCES devplat_workspace_members (id),
    CONSTRAINT ck_devplat_billing_accounts_class CHECK (classification IN ('RETAIL', 'CONTRACT', 'INTERNAL_COST_CENTER')),
    CONSTRAINT ck_devplat_billing_accounts_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'CLOSED')),
    CONSTRAINT ck_devplat_billing_accounts_currency CHECK (currency IS NULL OR currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_devplat_billing_accounts_country CHECK (country IS NULL OR country ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_devplat_billing_accounts_ext CHECK (external_billing_ref IS NULL OR external_billing_ref ~ '^[A-Za-z0-9._:/-]{1,128}$'),
    CONSTRAINT ck_devplat_billing_accounts_cc CHECK (cost_center_ref IS NULL OR cost_center_ref ~ '^[A-Za-z0-9._-]{1,64}$'),
    -- Internal cost allocation (first-party) is a cost-centre reference, never retail billing.
    CONSTRAINT ck_devplat_billing_accounts_internal CHECK ((classification = 'INTERNAL_COST_CENTER') = (cost_center_ref IS NOT NULL))
);

-- Immutable tenant-scoped activity history of human / operator control-plane mutations.
CREATE TABLE devplat_activity_events (
    id                  UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    actor_kind          VARCHAR(16)  NOT NULL,
    actor_ref           VARCHAR(100) NOT NULL,
    action              VARCHAR(64)  NOT NULL,
    target_type         VARCHAR(32)  NOT NULL,
    target_id           VARCHAR(64)  NOT NULL,
    outcome             VARCHAR(16)  NOT NULL,
    request_id          VARCHAR(64)  NOT NULL,
    correlation_id      VARCHAR(64)  NOT NULL,
    summary             JSONB        NOT NULL DEFAULT '{}'::jsonb,
    occurred_at         TIMESTAMPTZ  NOT NULL,
    seq                 BIGSERIAL    NOT NULL,
    CONSTRAINT pk_devplat_activity_events PRIMARY KEY (id),
    CONSTRAINT uq_devplat_activity_events_seq UNIQUE (seq),
    CONSTRAINT ck_devplat_activity_events_actor CHECK (actor_kind IN ('USER', 'OPERATOR', 'SERVICE')),
    CONSTRAINT ck_devplat_activity_events_action CHECK (action ~ '^[a-z][a-z_]*\.[a-z][a-z_.]*$'),
    CONSTRAINT ck_devplat_activity_events_outcome CHECK (outcome IN ('SUCCEEDED', 'DENIED', 'FAILED')),
    CONSTRAINT ck_devplat_activity_events_summary CHECK (
        jsonb_typeof(summary) = 'object' AND octet_length(summary::text) <= 2048
        AND NOT (summary ?| ARRAY['secret', 'signingSecret', 'credential', 'password', 'token', 'apiKey',
                                  'verifier', 'privateKey', 'phoneNumber', 'email', 'contact']))
);
CREATE INDEX ix_devplat_activity_events_tenant ON devplat_activity_events (tenant_id, seq DESC);
CREATE FUNCTION devplat_append_only() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '%: append-only', TG_TABLE_NAME USING ERRCODE = 'check_violation';
END $$;
CREATE TRIGGER tr_devplat_activity_events_append_only BEFORE UPDATE OR DELETE ON devplat_activity_events
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_activity_events_no_truncate BEFORE TRUNCATE ON devplat_activity_events
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();

-- Idempotent replay of control-plane mutations (scope: actor + route).
CREATE TABLE devplat_idempotency_records (
    scope_key           VARCHAR(200) NOT NULL,
    idempotency_key     VARCHAR(128) NOT NULL,
    request_digest      CHAR(64)     NOT NULL,
    response_status     SMALLINT     NOT NULL,
    response_body       JSONB        NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_idempotency_records PRIMARY KEY (scope_key, idempotency_key),
    CONSTRAINT ck_devplat_idempotency_records_key CHECK (idempotency_key ~ '^[\x21-\x7e]{1,128}$')
);
CREATE INDEX ix_devplat_idempotency_records_created ON devplat_idempotency_records (created_at);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_idempotency_records;
DROP TABLE IF EXISTS devplat_activity_events;
DROP TABLE IF EXISTS devplat_billing_accounts;
DROP TABLE IF EXISTS devplat_plan_assignments;
DROP TABLE IF EXISTS devplat_commercial_plans;
DROP TABLE IF EXISTS devplat_environment_profiles;
DROP TABLE IF EXISTS devplat_project_profiles;
DROP TABLE IF EXISTS devplat_invitations;
DROP TABLE IF EXISTS devplat_workspace_members;
DROP TABLE IF EXISTS devplat_organizations;
DROP FUNCTION IF EXISTS devplat_append_only();
DROP FUNCTION IF EXISTS devplat_no_delete();
DROP FUNCTION IF EXISTS devplat_commercial_plans_guard();
DROP FUNCTION IF EXISTS devplat_plan_assignments_guard();
DROP FUNCTION IF EXISTS devplat_environment_profiles_guard();
DROP FUNCTION IF EXISTS devplat_project_profiles_guard();
DROP FUNCTION IF EXISTS devplat_invitations_guard();
DROP FUNCTION IF EXISTS devplat_workspace_members_guard();
DROP FUNCTION IF EXISTS devplat_organizations_guard();
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
