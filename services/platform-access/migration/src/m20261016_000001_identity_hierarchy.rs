//! The shared platform identity hierarchy, owned by Platform Access:
//!
//! ```text
//! Tenant ─┬─ Application ─┬─ allowed scopes (optionally restricted to environment kinds)
//!         │               └─ Environment (SANDBOX | TEST | PRODUCTION)
//!         │                    ├─ Service Account ── API Credential ── granted scopes
//!         │                    └─ Principal (communication participant / entity)
//! ```
//!
//! The schema is the one Calling's migrations 000001–000003 / 000042 created when Calling
//! still owned this authority (Part III), so identifiers, verifier semantics, ownership
//! chains and invariants carry over unchanged during the controlled import
//! (`platform-access-admin identity import`). What is new:
//!
//! * every entity row carries a `revision` (1 at insert, +1 on every effective update,
//!   trigger-maintained) — consumers order projection updates by it;
//! * an application scope may be restricted to environment kinds
//!   (`allowed_environment_kinds`, NULL = every kind) and records its `grant_basis`;
//!   privileged operator scopes (`numbers.admin`) require `PRIVILEGED_OPERATOR`.
//!
//! Parent consistency is enforced by composite foreign keys on `(…ownership…, id)`;
//! ownership columns are immutable; REVOKED is terminal; the raw credential secret has no
//! column (only the keyed HMAC-SHA-256 verifier).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Shared invariant guards
-- ---------------------------------------------------------------------------
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

-- Effective updates bump `revision` by exactly one; a caller can never set it.
CREATE FUNCTION platform_bump_revision() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (to_jsonb(NEW) - 'revision' - 'updated_at') IS DISTINCT FROM (to_jsonb(OLD) - 'revision' - 'updated_at') THEN
        NEW.revision := OLD.revision + 1;
    ELSE
        NEW.revision := OLD.revision;
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION platform_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'platform invariant violated: % is append-only', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

-- ---------------------------------------------------------------------------
-- Tenant
-- ---------------------------------------------------------------------------
CREATE TABLE platform_tenants (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug             VARCHAR(63)  NOT NULL,
    display_name     VARCHAR(200) NOT NULL,
    classification   TEXT         NOT NULL,
    status           TEXT         NOT NULL DEFAULT 'ACTIVE',
    billing_currency CHAR(3)      NOT NULL DEFAULT 'USD',
    revision         BIGINT       NOT NULL DEFAULT 1,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT uq_platform_tenants_slug UNIQUE (slug),
    CONSTRAINT ck_platform_tenants_slug CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_platform_tenants_display_name CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    CONSTRAINT ck_platform_tenants_classification CHECK (classification IN ('FIRST_PARTY', 'EXTERNAL')),
    CONSTRAINT ck_platform_tenants_status CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    CONSTRAINT ck_platform_tenants_billing_currency CHECK (billing_currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_platform_tenants_revision CHECK (revision > 0)
);
CREATE TRIGGER trg_platform_tenants_immutable
    BEFORE UPDATE ON platform_tenants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'slug', 'classification', 'billing_currency', 'created_at');
CREATE TRIGGER trg_platform_tenants_revision
    BEFORE UPDATE ON platform_tenants
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

-- ---------------------------------------------------------------------------
-- Application
-- ---------------------------------------------------------------------------
CREATE TABLE platform_applications (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    UUID         NOT NULL,
    slug         VARCHAR(63)  NOT NULL,
    display_name VARCHAR(200) NOT NULL,
    status       TEXT         NOT NULL DEFAULT 'ACTIVE',
    revision     BIGINT       NOT NULL DEFAULT 1,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_applications_tenant
        FOREIGN KEY (tenant_id) REFERENCES platform_tenants (id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_applications_tenant_slug UNIQUE (tenant_id, slug),
    CONSTRAINT uq_platform_applications_owner_id UNIQUE (tenant_id, id),
    CONSTRAINT ck_platform_applications_slug CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_platform_applications_display_name CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    CONSTRAINT ck_platform_applications_status CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    CONSTRAINT ck_platform_applications_revision CHECK (revision > 0)
);
CREATE TRIGGER trg_platform_applications_immutable
    BEFORE UPDATE ON platform_applications
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'tenant_id', 'slug', 'created_at');
CREATE TRIGGER trg_platform_applications_revision
    BEFORE UPDATE ON platform_applications
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

-- ---------------------------------------------------------------------------
-- Environment: live_telecom_allowed can never be true outside PRODUCTION.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_environments (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            UUID        NOT NULL,
    application_id       UUID        NOT NULL,
    kind                 TEXT        NOT NULL,
    status               TEXT        NOT NULL DEFAULT 'ACTIVE',
    live_telecom_allowed BOOLEAN     NOT NULL DEFAULT FALSE,
    revision             BIGINT      NOT NULL DEFAULT 1,
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
        CHECK (kind = 'PRODUCTION' OR live_telecom_allowed = FALSE),
    CONSTRAINT ck_platform_environments_revision CHECK (revision > 0)
);
CREATE TRIGGER trg_platform_environments_immutable
    BEFORE UPDATE ON platform_environments
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'tenant_id', 'application_id', 'kind', 'created_at');
CREATE TRIGGER trg_platform_environments_revision
    BEFORE UPDATE ON platform_environments
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

-- ---------------------------------------------------------------------------
-- Service account
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
    revision       BIGINT       NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_service_accounts_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_service_accounts_environment_name UNIQUE (environment_id, name),
    CONSTRAINT uq_platform_service_accounts_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_platform_service_accounts_name CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_platform_service_accounts_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'REVOKED')),
    CONSTRAINT ck_platform_service_accounts_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_platform_service_accounts_revision CHECK (revision > 0)
);
CREATE INDEX idx_platform_service_accounts_application ON platform_service_accounts (application_id);
CREATE TRIGGER trg_platform_service_accounts_immutable
    BEFORE UPDATE ON platform_service_accounts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('id', 'tenant_id', 'application_id', 'environment_id', 'name', 'created_at');
CREATE TRIGGER trg_platform_service_accounts_revoked_terminal
    BEFORE UPDATE ON platform_service_accounts
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_leaving_terminal_status('REVOKED');
CREATE TRIGGER trg_platform_service_accounts_revision
    BEFORE UPDATE ON platform_service_accounts
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

-- ---------------------------------------------------------------------------
-- Application allowed scopes. Values are validated against the server-side scope
-- registry before they are written. `allowed_environment_kinds` NULL = every kind.
-- Privileged operator scopes need an explicit PRIVILEGED_OPERATOR grant basis.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_application_scopes (
    tenant_id                 UUID         NOT NULL,
    application_id            UUID         NOT NULL,
    scope                     VARCHAR(100) NOT NULL,
    allowed_environment_kinds TEXT[],
    grant_basis               TEXT         NOT NULL DEFAULT 'STANDARD',
    granted_at                TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_application_scopes PRIMARY KEY (application_id, scope),
    CONSTRAINT fk_platform_application_scopes_application
        FOREIGN KEY (tenant_id, application_id)
        REFERENCES platform_applications (tenant_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_application_scopes_scope
        CHECK (scope ~ '^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$'),
    CONSTRAINT ck_platform_application_scopes_environment_kinds CHECK (
        allowed_environment_kinds IS NULL
        OR (cardinality(allowed_environment_kinds) BETWEEN 1 AND 3
            AND allowed_environment_kinds <@ ARRAY['SANDBOX', 'TEST', 'PRODUCTION']::TEXT[])),
    CONSTRAINT ck_platform_application_scopes_grant_basis
        CHECK (grant_basis IN ('STANDARD', 'PRIVILEGED_OPERATOR')),
    CONSTRAINT ck_platform_application_scopes_privileged
        CHECK (scope <> 'numbers.admin' OR grant_basis = 'PRIVILEGED_OPERATOR')
);
CREATE INDEX idx_platform_application_scopes_tenant ON platform_application_scopes (tenant_id);
CREATE TRIGGER trg_platform_application_scopes_immutable
    BEFORE UPDATE ON platform_application_scopes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('tenant_id', 'application_id', 'scope', 'granted_at');

-- ---------------------------------------------------------------------------
-- API credentials: only the keyed verifier, never the secret.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_api_credentials (
    id                          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    public_id                   VARCHAR(32)  NOT NULL,
    tenant_id                   UUID         NOT NULL,
    application_id              UUID         NOT NULL,
    environment_id              UUID         NOT NULL,
    service_account_id          UUID         NOT NULL,
    label                       VARCHAR(100),
    verifier                    BYTEA        NOT NULL,
    verifier_algorithm          TEXT         NOT NULL,
    verifier_key_version        INTEGER      NOT NULL,
    last_four                   CHAR(4)      NOT NULL,
    status                      TEXT         NOT NULL DEFAULT 'ACTIVE',
    not_before                  TIMESTAMPTZ  NOT NULL,
    expires_at                  TIMESTAMPTZ  NOT NULL,
    revoked_at                  TIMESTAMPTZ,
    rotated_from_credential_id  UUID,
    superseded_by_credential_id UUID,
    superseded_at               TIMESTAMPTZ,
    supersession_valid_until    TIMESTAMPTZ,
    revision                    BIGINT       NOT NULL DEFAULT 1,
    created_at                  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_api_credentials_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_api_credentials_rotated_from
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id, rotated_from_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_api_credentials_superseded_by
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id, superseded_by_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_api_credentials_public_id UNIQUE (public_id),
    CONSTRAINT uq_platform_api_credentials_owner_id
        UNIQUE (tenant_id, application_id, environment_id, service_account_id, id),
    CONSTRAINT uq_platform_api_credentials_application_id UNIQUE (tenant_id, application_id, id),
    CONSTRAINT uq_platform_api_credentials_rotated_from UNIQUE (rotated_from_credential_id),
    CONSTRAINT uq_platform_api_credentials_superseded_by UNIQUE (superseded_by_credential_id),
    CONSTRAINT ck_platform_api_credentials_public_id CHECK (public_id ~ '^[0-9a-f]{24}$'),
    CONSTRAINT ck_platform_api_credentials_verifier_length CHECK (octet_length(verifier) = 32),
    CONSTRAINT ck_platform_api_credentials_verifier_algorithm CHECK (verifier_algorithm IN ('HMAC_SHA256')),
    CONSTRAINT ck_platform_api_credentials_verifier_key_version CHECK (verifier_key_version > 0),
    CONSTRAINT ck_platform_api_credentials_last_four CHECK (last_four ~ '^[0-9a-f]{4}$'),
    CONSTRAINT ck_platform_api_credentials_label CHECK (label IS NULL OR length(btrim(label)) BETWEEN 1 AND 100),
    CONSTRAINT ck_platform_api_credentials_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_platform_api_credentials_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_platform_api_credentials_validity_window CHECK (expires_at > not_before),
    CONSTRAINT ck_platform_api_credentials_no_self_link
        CHECK (rotated_from_credential_id IS DISTINCT FROM id AND superseded_by_credential_id IS DISTINCT FROM id),
    CONSTRAINT ck_platform_api_credentials_supersession_complete
        CHECK ((superseded_by_credential_id IS NULL) = (superseded_at IS NULL)
               AND (superseded_at IS NULL) = (supersession_valid_until IS NULL)),
    CONSTRAINT ck_platform_api_credentials_supersession_never_extends
        CHECK (supersession_valid_until IS NULL OR supersession_valid_until <= expires_at),
    CONSTRAINT ck_platform_api_credentials_revision CHECK (revision > 0)
);
CREATE INDEX idx_platform_api_credentials_service_account ON platform_api_credentials (service_account_id);
CREATE INDEX idx_platform_api_credentials_application_environment
    ON platform_api_credentials (application_id, environment_id);
CREATE TRIGGER trg_platform_api_credentials_immutable
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'public_id', 'tenant_id', 'application_id', 'environment_id', 'service_account_id',
        'verifier', 'verifier_algorithm', 'verifier_key_version', 'last_four',
        'not_before', 'expires_at', 'rotated_from_credential_id', 'created_at');
CREATE TRIGGER trg_platform_api_credentials_set_once
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'superseded_by_credential_id', 'superseded_at', 'supersession_valid_until', 'revoked_at');
CREATE TRIGGER trg_platform_api_credentials_revoked_terminal
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_leaving_terminal_status('REVOKED');
CREATE TRIGGER trg_platform_api_credentials_revision
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

CREATE TABLE platform_api_credential_scopes (
    credential_id  UUID         NOT NULL,
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    scope          VARCHAR(100) NOT NULL,
    granted_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_api_credential_scopes PRIMARY KEY (credential_id, scope),
    CONSTRAINT fk_platform_api_credential_scopes_credential
        FOREIGN KEY (tenant_id, application_id, credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, id) ON DELETE CASCADE,
    CONSTRAINT fk_platform_api_credential_scopes_application_scope
        FOREIGN KEY (application_id, scope)
        REFERENCES platform_application_scopes (application_id, scope) ON DELETE CASCADE
);
CREATE INDEX idx_platform_api_credential_scopes_application_scope
    ON platform_api_credential_scopes (application_id, scope);
CREATE TRIGGER trg_platform_api_credential_scopes_immutable
    BEFORE UPDATE ON platform_api_credential_scopes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'credential_id', 'tenant_id', 'application_id', 'scope', 'granted_at');

-- ---------------------------------------------------------------------------
-- Communication principals (environment-scoped). Plaintext contact data is
-- structurally excluded. A tombstoned principal (`tombstoned_at`) is DEACTIVATED for
-- ever and carries a non-identifying reference derived from its own id.
-- ---------------------------------------------------------------------------
CREATE TABLE platform_principals (
    id                        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id                 UUID         NOT NULL,
    application_id            UUID         NOT NULL,
    environment_id            UUID         NOT NULL,
    kind                      TEXT         NOT NULL,
    external_reference        VARCHAR(255) NOT NULL,
    linked_service_account_id UUID,
    status                    TEXT         NOT NULL DEFAULT 'ACTIVE',
    tombstoned_at             TIMESTAMPTZ,
    revision                  BIGINT       NOT NULL DEFAULT 1,
    created_at                TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_principals_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_principals_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, linked_service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_principals_scoped_reference
        UNIQUE (tenant_id, application_id, environment_id, kind, external_reference),
    CONSTRAINT uq_platform_principals_owner_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_platform_principals_linked_service_account UNIQUE (linked_service_account_id),
    CONSTRAINT ck_platform_principals_kind CHECK (kind IN (
        'NATIVE_CF_USER',
        'EXTERNAL_APPLICATION_PRINCIPAL',
        'ENTERPRISE_MEMBER',
        'SERVICE_ACCOUNT',
        'TELEPHONE_ENDPOINT',
        'TEMPORARY_EXTERNAL_PARTICIPANT',
        'BOT_OR_SERVICE')),
    CONSTRAINT ck_platform_principals_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'DEACTIVATED')),
    CONSTRAINT ck_platform_principals_reference_format CHECK (external_reference ~ '^[!-~]{1,255}$'),
    CONSTRAINT ck_platform_principals_reference_not_telephone
        CHECK (external_reference !~ '\+[0-9]' AND external_reference !~* '^(tel|sip|sips):'),
    CONSTRAINT ck_platform_principals_telephone_endpoint_opaque
        CHECK (kind <> 'TELEPHONE_ENDPOINT'
               OR external_reference ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    CONSTRAINT ck_platform_principals_service_account_link
        CHECK ((kind = 'SERVICE_ACCOUNT') = (linked_service_account_id IS NOT NULL)),
    CONSTRAINT ck_platform_principals_service_account_reference
        CHECK (kind <> 'SERVICE_ACCOUNT' OR external_reference = linked_service_account_id::text),
    CONSTRAINT ck_platform_principals_tombstoned CHECK (tombstoned_at IS NULL OR status = 'DEACTIVATED'),
    CONSTRAINT ck_platform_principals_revision CHECK (revision > 0)
);
CREATE INDEX idx_platform_principals_environment ON platform_principals (environment_id);
CREATE TRIGGER trg_platform_principals_immutable
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'kind',
        'linked_service_account_id', 'created_at');
CREATE TRIGGER trg_platform_principals_tombstoned_once
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('tombstoned_at');
CREATE FUNCTION platform_guard_principal_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.external_reference IS DISTINCT FROM OLD.external_reference AND NOT (
        OLD.tombstoned_at IS NULL AND NEW.tombstoned_at IS NOT NULL
        AND NEW.external_reference = CASE WHEN NEW.kind = 'TELEPHONE_ENDPOINT' THEN NEW.id::text
                                          ELSE 'tombstoned:' || NEW.id::text END) THEN
        RAISE EXCEPTION 'platform invariant violated: %.external_reference is immutable (replaced once by a tombstone only)', TG_TABLE_NAME
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF OLD.tombstoned_at IS NOT NULL AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'platform invariant violated: a tombstoned principal cannot change status'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_platform_principals_reference_tombstone_only
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_guard_principal_reference();
CREATE TRIGGER trg_platform_principals_revision
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_principals;
DROP FUNCTION IF EXISTS platform_guard_principal_reference();
DROP TABLE IF EXISTS platform_api_credential_scopes;
DROP TABLE IF EXISTS platform_api_credentials;
DROP TABLE IF EXISTS platform_application_scopes;
DROP TABLE IF EXISTS platform_service_accounts;
DROP TABLE IF EXISTS platform_environments;
DROP TABLE IF EXISTS platform_applications;
DROP TABLE IF EXISTS platform_tenants;
DROP FUNCTION IF EXISTS platform_forbid_mutation();
DROP FUNCTION IF EXISTS platform_bump_revision();
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
