//! Operational records of Platform Access.
//!
//! * `platform_access_audit`: operator audit of every identity administration command
//!   (who, what, which target, when), written in the same transaction as the mutation;
//!   bounded detail without secrets; append-only.
//! * `platform_auth_failure_windows`: bounded failed-authentication counters per presented
//!   credential key id (unknown key ids share ONE bucket); unlogged, pruned periodically.
//! * `platform_identity_imports`: receipts of the controlled identity import from the
//!   previous authority (Calling, Part III). A receipt records the bundle digest, the
//!   per-entity counts and a digest of the imported credential verifiers so the source
//!   can verify the hand-off without any raw secret ever existing in either service.
//! * `platform_credential_principal_restrictions`: authorized-principal restrictions. A
//!   credential with at least one row may act only for the listed principals of its own
//!   environment (composite foreign keys pin the scope).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_access_audit (
    id             UUID         NOT NULL,
    actor          VARCHAR(100) NOT NULL,
    correlation_id VARCHAR(64)  NOT NULL,
    action         VARCHAR(64)  NOT NULL,
    target_kind    VARCHAR(32)  NOT NULL,
    target_id      UUID,
    tenant_id      UUID,
    application_id UUID,
    environment_id UUID,
    detail         JSONB        NOT NULL DEFAULT '{}'::jsonb,
    occurred_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_access_audit PRIMARY KEY (id),
    CONSTRAINT ck_platform_access_audit_actor CHECK (actor ~ '^[A-Za-z0-9][A-Za-z0-9._@-]{0,99}$'),
    CONSTRAINT ck_platform_access_audit_correlation CHECK (correlation_id ~ '^[A-Za-z0-9_-]{8,64}$'),
    CONSTRAINT ck_platform_access_audit_action CHECK (action IN (
        'TENANT_CREATED', 'TENANT_STATUS_SET',
        'APPLICATION_CREATED', 'APPLICATION_STATUS_SET',
        'ENVIRONMENT_CREATED', 'ENVIRONMENT_STATUS_SET', 'ENVIRONMENT_LIVE_TELECOM_SET',
        'SERVICE_ACCOUNT_CREATED', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'CREDENTIAL_PRINCIPALS_RESTRICTED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'PRIVILEGED_SCOPE_GRANTED',
        'PRINCIPAL_REGISTERED', 'PRINCIPAL_STATUS_SET', 'PRINCIPAL_TOMBSTONED',
        'IDENTITY_IMPORTED')),
    CONSTRAINT ck_platform_access_audit_target_kind CHECK (target_kind IN (
        'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
        'IDENTITY_IMPORT')),
    CONSTRAINT ck_platform_access_audit_detail CHECK (
        jsonb_typeof(detail) = 'object' AND octet_length(detail::text) <= 2048),
    CONSTRAINT ck_platform_access_audit_detail_no_secret CHECK (
        NOT (detail ? 'secret' OR detail ? 'pepper' OR detail ? 'verifier' OR detail ? 'plaintext'))
);
CREATE INDEX idx_platform_access_audit_target ON platform_access_audit (target_kind, target_id, occurred_at DESC);
CREATE INDEX idx_platform_access_audit_tenant ON platform_access_audit (tenant_id, occurred_at DESC);
CREATE INDEX idx_platform_access_audit_correlation ON platform_access_audit (correlation_id);
CREATE TRIGGER trg_platform_access_audit_append_only
    BEFORE UPDATE OR DELETE ON platform_access_audit
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
CREATE TRIGGER trg_platform_access_audit_no_truncate
    BEFORE TRUNCATE ON platform_access_audit
    FOR EACH STATEMENT EXECUTE FUNCTION platform_forbid_mutation();

CREATE UNLOGGED TABLE platform_auth_failure_windows (
    subject_key  VARCHAR(32) NOT NULL,
    window_start TIMESTAMPTZ NOT NULL,
    failures     BIGINT      NOT NULL,
    CONSTRAINT pk_platform_auth_failure_windows PRIMARY KEY (subject_key, window_start),
    CONSTRAINT ck_platform_auth_failure_windows_subject
        CHECK (subject_key ~ '^[0-9a-f]{24}$' OR subject_key = 'unknown'),
    CONSTRAINT ck_platform_auth_failure_windows_failures CHECK (failures > 0)
);
CREATE INDEX idx_platform_auth_failure_windows_start ON platform_auth_failure_windows (window_start);

CREATE TABLE platform_identity_imports (
    id                      UUID         NOT NULL,
    source_service          VARCHAR(64)  NOT NULL,
    bundle_digest           CHAR(64)     NOT NULL,
    verifier_digest         CHAR(64)     NOT NULL,
    counts                  JSONB        NOT NULL,
    actor                   VARCHAR(100) NOT NULL,
    imported_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_identity_imports PRIMARY KEY (id),
    CONSTRAINT uq_platform_identity_imports_digest UNIQUE (bundle_digest),
    CONSTRAINT ck_platform_identity_imports_source CHECK (source_service ~ '^cfmeet\.[a-z][a-z0-9-]*$'),
    CONSTRAINT ck_platform_identity_imports_digests CHECK (
        bundle_digest ~ '^[0-9a-f]{64}$' AND verifier_digest ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_platform_identity_imports_counts CHECK (jsonb_typeof(counts) = 'object')
);
CREATE TRIGGER trg_platform_identity_imports_append_only
    BEFORE UPDATE OR DELETE ON platform_identity_imports
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();

CREATE TABLE platform_credential_principal_restrictions (
    credential_id  UUID        NOT NULL,
    principal_id   UUID        NOT NULL,
    tenant_id      UUID        NOT NULL,
    application_id UUID        NOT NULL,
    environment_id UUID        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_credential_principal_restrictions PRIMARY KEY (credential_id, principal_id),
    CONSTRAINT fk_platform_credential_principal_restrictions_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_credential_principal_restrictions_credential
        FOREIGN KEY (tenant_id, application_id, credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, id) ON DELETE CASCADE
);
CREATE TRIGGER trg_platform_credential_principal_restrictions_immutable
    BEFORE UPDATE ON platform_credential_principal_restrictions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
-- The credential's environment must be the principal's environment.
CREATE FUNCTION platform_guard_restriction_environment() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM platform_api_credentials c
                   WHERE c.id = NEW.credential_id AND c.environment_id = NEW.environment_id) THEN
        RAISE EXCEPTION 'platform invariant violated: a principal restriction must stay inside the credential environment'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_platform_credential_principal_restrictions_environment
    BEFORE INSERT ON platform_credential_principal_restrictions
    FOR EACH ROW EXECUTE FUNCTION platform_guard_restriction_environment();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_credential_principal_restrictions;
DROP FUNCTION IF EXISTS platform_guard_restriction_environment();
DROP TABLE IF EXISTS platform_identity_imports;
DROP TABLE IF EXISTS platform_auth_failure_windows;
DROP TABLE IF EXISTS platform_access_audit;
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
