//! Enterprise calling foundation (Part F §52–§53, §18–§23).
//!
//! The organization boundary of enterprise calling is the existing tenant / application /
//! environment triple (ADR 0018): every enterprise row carries it and references
//! `platform_environments` with a composite foreign key, and the ownership columns are
//! immutable. No second "organization" identity exists.
//!
//! * `enterprise_call_policies` — the TYPED enterprise call policy (§18). Every hook is
//!   its own validated column; there is no policy blob. At most one ACTIVE default per
//!   environment; without one the service applies the restrictive built-in default.
//! * `enterprise_departments` / `enterprise_department_members` — administrative and
//!   routing groups of principals of the same scope (§53, §145). Not an authorization
//!   system: membership grants nothing by itself.
//! * `enterprise_admin_audit` — durable evidence of every administrative change (§21):
//!   actor (customer application credential, operator CLI or a system worker), scope, resource, operation, old / new version, request / correlation ids,
//!   time and a bounded safe detail object. Append-only.
//!
//! Mutable configuration resources carry `version >= 1`; the service bumps it exactly
//! once per change and refuses a stale `expectedVersion` (409).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- Append-only enforcement shared by Part F evidence tables.
CREATE FUNCTION enterprise_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'enterprise invariant violated: % is append-only', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

-- Version discipline of mutable enterprise configuration: an UPDATE that changes the row
-- must advance version by exactly one.
CREATE FUNCTION enterprise_guard_version() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'enterprise invariant violated: %.version must advance by one', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE enterprise_call_policies (
    id                              UUID         NOT NULL,
    tenant_id                       UUID         NOT NULL,
    application_id                  UUID         NOT NULL,
    environment_id                  UUID         NOT NULL,
    name                            VARCHAR(100) NOT NULL,
    is_default                      BOOLEAN      NOT NULL,
    status                          TEXT         NOT NULL,
    internal_calling_enabled        BOOLEAN      NOT NULL,
    external_public_calling_allowed BOOLEAN      NOT NULL,
    public_destination_countries    TEXT[]       NOT NULL,
    forwarding_allowed              BOOLEAN      NOT NULL,
    public_forwarding_allowed       BOOLEAN      NOT NULL,
    spend_policy                    TEXT         NOT NULL,
    voicemail_allowed               BOOLEAN      NOT NULL,
    transfer_allowed                BOOLEAN      NOT NULL,
    max_ring_group_fanout           SMALLINT     NOT NULL,
    max_queue_capacity              INTEGER      NOT NULL,
    max_queue_wait_seconds          INTEGER      NOT NULL,
    max_routing_hops                SMALLINT     NOT NULL,
    extension_format                TEXT         NOT NULL,
    extension_min_length            SMALLINT     NOT NULL,
    extension_max_length            SMALLINT     NOT NULL,
    admin_capabilities              TEXT[]       NOT NULL,
    version                         BIGINT       NOT NULL,
    created_at                      TIMESTAMPTZ  NOT NULL,
    updated_at                      TIMESTAMPTZ  NOT NULL,
    disabled_at                     TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_call_policies PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_call_policies_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_call_policies_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_call_policies_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_call_policies_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_call_policies_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_call_policies_default_active
        CHECK (NOT is_default OR status = 'ACTIVE'),
    CONSTRAINT ck_enterprise_call_policies_countries CHECK (
        cardinality(public_destination_countries) <= 300
        AND array_to_string(public_destination_countries, ',') ~ '^([A-Z]{2}(,[A-Z]{2})*)?$'),
    CONSTRAINT ck_enterprise_call_policies_public_forwarding
        CHECK (NOT public_forwarding_allowed OR (forwarding_allowed AND external_public_calling_allowed)),
    CONSTRAINT ck_enterprise_call_policies_spend CHECK (spend_policy IN ('APPLICATION_TELECOM_LIMITS')),
    CONSTRAINT ck_enterprise_call_policies_fanout CHECK (max_ring_group_fanout BETWEEN 1 AND 15),
    CONSTRAINT ck_enterprise_call_policies_queue_capacity CHECK (max_queue_capacity BETWEEN 1 AND 1000),
    CONSTRAINT ck_enterprise_call_policies_queue_wait CHECK (max_queue_wait_seconds BETWEEN 10 AND 3600),
    CONSTRAINT ck_enterprise_call_policies_hops CHECK (max_routing_hops BETWEEN 1 AND 16),
    CONSTRAINT ck_enterprise_call_policies_extension_format
        CHECK (extension_format IN ('NUMERIC', 'ALPHANUMERIC')),
    CONSTRAINT ck_enterprise_call_policies_extension_length CHECK (
        extension_min_length >= 2 AND extension_min_length <= extension_max_length
        AND extension_max_length <= CASE extension_format WHEN 'NUMERIC' THEN 6 ELSE 32 END),
    CONSTRAINT ck_enterprise_call_policies_admin_capabilities CHECK (
        admin_capabilities <@ ARRAY['MANAGE_ROUTING', 'MANAGE_PUBLIC_FORWARDING',
                                    'MANAGE_VOICEMAIL', 'MANAGE_RECORDING']::TEXT[]),
    CONSTRAINT ck_enterprise_call_policies_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_call_policies_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_call_policies_default
    ON enterprise_call_policies (environment_id) WHERE is_default;
CREATE UNIQUE INDEX uq_enterprise_call_policies_active_name
    ON enterprise_call_policies (environment_id, lower(name)) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_call_policies_scope
    ON enterprise_call_policies (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_call_policies_immutable
    BEFORE UPDATE ON enterprise_call_policies
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_call_policies_version
    BEFORE UPDATE ON enterprise_call_policies
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

CREATE TABLE enterprise_departments (
    id             UUID         NOT NULL,
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    environment_id UUID         NOT NULL,
    name           VARCHAR(100) NOT NULL,
    description    VARCHAR(500),
    status         TEXT         NOT NULL,
    version        BIGINT       NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL,
    updated_at     TIMESTAMPTZ  NOT NULL,
    disabled_at    TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_departments PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_departments_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_departments_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_departments_name CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_departments_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_departments_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_departments_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_departments_timestamps CHECK (updated_at >= created_at)
);
CREATE UNIQUE INDEX uq_enterprise_departments_active_name
    ON enterprise_departments (environment_id, lower(name)) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_departments_scope
    ON enterprise_departments (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_departments_immutable
    BEFORE UPDATE ON enterprise_departments
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_enterprise_departments_version
    BEFORE UPDATE ON enterprise_departments
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

CREATE TABLE enterprise_department_members (
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    department_id  UUID        NOT NULL,
    principal_id   UUID        NOT NULL,
    role           TEXT        NOT NULL,
    added_at       TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_enterprise_department_members PRIMARY KEY (department_id, principal_id),
    CONSTRAINT fk_enterprise_department_members_department
        FOREIGN KEY (tenant_id, application_id, environment_id, department_id)
        REFERENCES enterprise_departments (tenant_id, application_id, environment_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_enterprise_department_members_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_department_members_role CHECK (role IN ('MEMBER', 'MANAGER'))
);
CREATE INDEX idx_enterprise_department_members_principal
    ON enterprise_department_members (principal_id);

CREATE TABLE enterprise_admin_audit (
    id                       UUID         NOT NULL,
    seq                      BIGINT       GENERATED ALWAYS AS IDENTITY,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    actor_kind               TEXT         NOT NULL,
    actor_service_account_id UUID,
    actor_credential_id      UUID,
    resource_type            TEXT         NOT NULL,
    resource_id              UUID         NOT NULL,
    operation                TEXT         NOT NULL,
    old_version              BIGINT,
    new_version              BIGINT,
    request_id               VARCHAR(128) NOT NULL,
    correlation_id           VARCHAR(128) NOT NULL,
    details                  JSONB        NOT NULL,
    occurred_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_enterprise_admin_audit PRIMARY KEY (id),
    CONSTRAINT fk_enterprise_admin_audit_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_enterprise_admin_audit_actor
        FOREIGN KEY (tenant_id, application_id, environment_id, actor_service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_enterprise_admin_audit_actor_kind
        CHECK (actor_kind IN ('CUSTOMER_APPLICATION', 'OPERATOR', 'SYSTEM')),
    CONSTRAINT ck_enterprise_admin_audit_customer_actor
        CHECK ((actor_kind = 'CUSTOMER_APPLICATION') = (actor_service_account_id IS NOT NULL)),
    CONSTRAINT ck_enterprise_admin_audit_resource_type CHECK (resource_type ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_enterprise_admin_audit_operation CHECK (operation ~ '^[A-Z][A-Z_]{1,63}$'),
    CONSTRAINT ck_enterprise_admin_audit_versions CHECK (
        (old_version IS NULL OR old_version >= 1) AND (new_version IS NULL OR new_version >= 1)),
    CONSTRAINT ck_enterprise_admin_audit_details CHECK (
        jsonb_typeof(details) = 'object' AND octet_length(details::text) <= 4096)
);
CREATE UNIQUE INDEX uq_enterprise_admin_audit_seq ON enterprise_admin_audit (seq);
CREATE INDEX idx_enterprise_admin_audit_scope_seq
    ON enterprise_admin_audit (tenant_id, application_id, environment_id, seq DESC);
CREATE INDEX idx_enterprise_admin_audit_resource ON enterprise_admin_audit (resource_id, seq DESC);
CREATE TRIGGER trg_enterprise_admin_audit_append_only
    BEFORE UPDATE OR DELETE ON enterprise_admin_audit
    FOR EACH ROW EXECUTE FUNCTION enterprise_forbid_mutation();
CREATE TRIGGER trg_enterprise_admin_audit_no_truncate
    BEFORE TRUNCATE ON enterprise_admin_audit
    FOR EACH STATEMENT EXECUTE FUNCTION enterprise_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS enterprise_admin_audit;
DROP TABLE IF EXISTS enterprise_department_members;
DROP TABLE IF EXISTS enterprise_departments;
DROP TABLE IF EXISTS enterprise_call_policies;
DROP FUNCTION IF EXISTS enterprise_guard_version();
DROP FUNCTION IF EXISTS enterprise_forbid_mutation();
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
