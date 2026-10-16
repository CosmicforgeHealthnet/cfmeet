//! Platform consumer hierarchy: Tenant → Application → Environment → Service Account,
//! plus the server-authoritative set of scopes each Application may grant.
//!
//! Parent consistency is enforced by the database, not only by service code: every child
//! row carries its full ownership chain and references its parent through a composite
//! foreign key on `(…ownership…, id)`, so a row can never point at a parent that belongs
//! to a different tenant / application / environment. Ownership columns are immutable
//! (trigger-enforced), so a row can never be re-parented after creation either.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Shared invariant guards
-- ---------------------------------------------------------------------------

-- Rejects any UPDATE that changes one of the columns named in the trigger arguments.
CREATE FUNCTION platform_forbid_column_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col     text;
    old_row jsonb := to_jsonb(OLD);
    new_row jsonb := to_jsonb(NEW);
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (new_row -> col) IS DISTINCT FROM (old_row -> col) THEN
            RAISE EXCEPTION 'platform invariant violated: %.% is immutable', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- Columns that may be set once (NULL → value) and never changed afterwards.
CREATE FUNCTION platform_forbid_reassignment() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    col     text;
    old_row jsonb := to_jsonb(OLD);
    new_row jsonb := to_jsonb(NEW);
BEGIN
    FOREACH col IN ARRAY TG_ARGV LOOP
        IF (old_row -> col) IS DISTINCT FROM 'null'::jsonb
           AND (new_row -> col) IS DISTINCT FROM (old_row -> col) THEN
            RAISE EXCEPTION 'platform invariant violated: %.% cannot be reassigned', TG_TABLE_NAME, col
                USING ERRCODE = 'integrity_constraint_violation';
        END IF;
    END LOOP;
    RETURN NEW;
END;
$$;

-- A row whose status equals TG_ARGV[0] may never leave that (terminal) status.
CREATE FUNCTION platform_forbid_leaving_terminal_status() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = TG_ARGV[0] AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'platform invariant violated: %.status % is terminal', TG_TABLE_NAME, OLD.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- Tenant: top-level security, ownership, policy and usage boundary.
-- FIRST_PARTY is a trust classification, never a product-specific code path.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_tenants (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug             VARCHAR(63)  NOT NULL,
    display_name     VARCHAR(200) NOT NULL,
    classification   TEXT         NOT NULL,
    status           TEXT         NOT NULL DEFAULT 'ACTIVE',
    -- Currency of every telecom-spend limit configured under this tenant (ISO 4217).
    billing_currency CHAR(3)      NOT NULL DEFAULT 'USD',
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT uq_platform_tenants_slug UNIQUE (slug),
    CONSTRAINT ck_platform_tenants_slug CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_platform_tenants_display_name CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    CONSTRAINT ck_platform_tenants_classification CHECK (classification IN ('FIRST_PARTY', 'EXTERNAL')),
    CONSTRAINT ck_platform_tenants_status CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    CONSTRAINT ck_platform_tenants_billing_currency CHECK (billing_currency ~ '^[A-Z]{3}$')
);
CREATE TRIGGER trg_platform_tenants_immutable
    BEFORE UPDATE ON platform_tenants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'slug', 'classification', 'billing_currency', 'created_at');

-- ---------------------------------------------------------------------------
-- Application / Project: belongs to exactly one tenant.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_applications (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    UUID         NOT NULL,
    slug         VARCHAR(63)  NOT NULL,
    display_name VARCHAR(200) NOT NULL,
    status       TEXT         NOT NULL DEFAULT 'ACTIVE',
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_applications_tenant
        FOREIGN KEY (tenant_id) REFERENCES platform_tenants (id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_applications_tenant_slug UNIQUE (tenant_id, slug),
    -- Composite target so children can prove (tenant, application) consistency.
    CONSTRAINT uq_platform_applications_owner_id UNIQUE (tenant_id, id),
    CONSTRAINT ck_platform_applications_slug CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_platform_applications_display_name CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    CONSTRAINT ck_platform_applications_status CHECK (status IN ('ACTIVE', 'SUSPENDED'))
);
CREATE TRIGGER trg_platform_applications_immutable
    BEFORE UPDATE ON platform_applications
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'tenant_id', 'slug', 'created_at');

-- ---------------------------------------------------------------------------
-- Application environment: SANDBOX / TEST / PRODUCTION, one of each per application.
-- live_telecom_allowed is the server-authoritative guard against real public-network
-- telecom effects; it can never be true outside PRODUCTION.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_environments (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            UUID        NOT NULL,
    application_id       UUID        NOT NULL,
    kind                 TEXT        NOT NULL,
    status               TEXT        NOT NULL DEFAULT 'ACTIVE',
    live_telecom_allowed BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_environments_application
        FOREIGN KEY (tenant_id, application_id)
        REFERENCES platform_applications (tenant_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_environments_application_kind UNIQUE (application_id, kind),
    CONSTRAINT uq_platform_environments_owner_id UNIQUE (tenant_id, application_id, id),
    CONSTRAINT ck_platform_environments_kind CHECK (kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_platform_environments_status CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    CONSTRAINT ck_platform_environments_live_telecom
        CHECK (kind = 'PRODUCTION' OR live_telecom_allowed = FALSE)
);
CREATE TRIGGER trg_platform_environments_immutable
    BEFORE UPDATE ON platform_environments
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'tenant_id', 'application_id', 'kind', 'created_at');

-- ---------------------------------------------------------------------------
-- Service account: the machine identity, bound to exactly one application environment.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_service_accounts (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    environment_id UUID         NOT NULL,
    name           VARCHAR(63)  NOT NULL,
    description    VARCHAR(500),
    status         TEXT         NOT NULL DEFAULT 'ACTIVE',
    revoked_at     TIMESTAMPTZ,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_service_accounts_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_service_accounts_environment_name UNIQUE (environment_id, name),
    CONSTRAINT uq_platform_service_accounts_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_platform_service_accounts_name CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_platform_service_accounts_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'REVOKED')),
    CONSTRAINT ck_platform_service_accounts_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL))
);
CREATE INDEX idx_platform_service_accounts_application ON platform_service_accounts (application_id);
CREATE TRIGGER trg_platform_service_accounts_immutable
    BEFORE UPDATE ON platform_service_accounts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'tenant_id', 'application_id', 'environment_id', 'name', 'created_at');
CREATE TRIGGER trg_platform_service_accounts_revoked_terminal
    BEFORE UPDATE ON platform_service_accounts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_leaving_terminal_status('REVOKED');

-- ---------------------------------------------------------------------------
-- Scopes an application may grant to its credentials. Values are validated against the
-- service's server-side scope registry before they are written here.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_application_scopes (
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    scope          VARCHAR(100) NOT NULL,
    granted_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_application_scopes PRIMARY KEY (application_id, scope),
    CONSTRAINT fk_platform_application_scopes_application
        FOREIGN KEY (tenant_id, application_id)
        REFERENCES platform_applications (tenant_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_application_scopes_scope
        CHECK (scope ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$')
);
CREATE INDEX idx_platform_application_scopes_tenant ON platform_application_scopes (tenant_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_application_scopes;
DROP TABLE IF EXISTS platform_service_accounts;
DROP TABLE IF EXISTS platform_environments;
DROP TABLE IF EXISTS platform_applications;
DROP TABLE IF EXISTS platform_tenants;
DROP FUNCTION IF EXISTS platform_forbid_leaving_terminal_status();
DROP FUNCTION IF EXISTS platform_forbid_reassignment();
DROP FUNCTION IF EXISTS platform_forbid_column_change();
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
