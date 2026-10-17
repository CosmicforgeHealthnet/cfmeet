//! Identity change feed: the transactional outbox of Platform Access.
//!
//! Every INSERT / UPDATE / DELETE of an identity row appends one row to
//! `platform_identity_changes` IN THE SAME TRANSACTION, written by a database trigger so
//! no code path can change identity without publishing the change. Consumers (Calling's
//! identity projection, later Numbering) read the feed through the authenticated
//! internal API (`GET /internal/v1/identity/changes?after=<sequence>`) — never this
//! table — and apply snapshots ordered by `revision`.
//!
//! * `snapshot` is the row after the change (before it, for a DELETE) WITHOUT verifier
//!   material: `verifier`, `verifier_algorithm` and `verifier_key_version` are removed by
//!   the trigger, so credential verifiers never leave Platform Access through the feed.
//! * `sequence` is allocated under a transaction-scoped advisory lock, so a committed
//!   sequence number is never overtaken by a lower one committing later: a consumer
//!   reading `sequence > cursor` cannot skip a change (rolled-back allocations only
//!   leave gaps).
//! * The feed is append-only (UPDATE / DELETE / TRUNCATE refused).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_identity_changes (
    sequence     BIGSERIAL    PRIMARY KEY,
    entity_kind  VARCHAR(32)  NOT NULL,
    operation    VARCHAR(8)   NOT NULL,
    entity_key   VARCHAR(160) NOT NULL,
    tenant_id    UUID,
    revision     BIGINT,
    snapshot     JSONB        NOT NULL,
    occurred_at  TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_platform_identity_changes_kind CHECK (entity_kind IN (
        'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'APPLICATION_SCOPE',
        'CREDENTIAL', 'CREDENTIAL_SCOPE', 'PRINCIPAL')),
    CONSTRAINT ck_platform_identity_changes_operation CHECK (operation IN ('INSERT', 'UPDATE', 'DELETE')),
    CONSTRAINT ck_platform_identity_changes_snapshot CHECK (
        jsonb_typeof(snapshot) = 'object'
        AND NOT (snapshot ? 'verifier' OR snapshot ? 'verifier_algorithm' OR snapshot ? 'verifier_key_version'))
);
CREATE INDEX idx_platform_identity_changes_tenant ON platform_identity_changes (tenant_id, sequence);
CREATE TRIGGER trg_platform_identity_changes_append_only
    BEFORE UPDATE OR DELETE ON platform_identity_changes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
CREATE TRIGGER trg_platform_identity_changes_no_truncate
    BEFORE TRUNCATE ON platform_identity_changes
    FOR EACH STATEMENT EXECUTE FUNCTION platform_forbid_mutation();

-- TG_ARGV[0] = entity kind, TG_ARGV[1] = key expression kind ('id' | 'application_scope' |
-- 'credential_scope').
CREATE FUNCTION platform_record_identity_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    row_data jsonb;
    key      text;
BEGIN
    IF TG_OP = 'UPDATE' AND to_jsonb(NEW) = to_jsonb(OLD) THEN
        RETURN NULL;
    END IF;
    IF TG_OP = 'DELETE' THEN
        row_data := to_jsonb(OLD);
    ELSE
        row_data := to_jsonb(NEW);
    END IF;
    row_data := row_data - 'verifier' - 'verifier_algorithm' - 'verifier_key_version';
    key := CASE TG_ARGV[1]
        WHEN 'application_scope' THEN (row_data ->> 'application_id') || ':' || (row_data ->> 'scope')
        WHEN 'credential_scope' THEN (row_data ->> 'credential_id') || ':' || (row_data ->> 'scope')
        ELSE row_data ->> 'id'
    END;
    -- Commit order == sequence order (see the module documentation).
    PERFORM pg_advisory_xact_lock(7239001);
    INSERT INTO platform_identity_changes (entity_kind, operation, entity_key, tenant_id, revision, snapshot)
    VALUES (TG_ARGV[0], TG_OP, key,
            CASE WHEN TG_ARGV[0] = 'TENANT' THEN (row_data ->> 'id')::uuid ELSE (row_data ->> 'tenant_id')::uuid END,
            (row_data ->> 'revision')::bigint,
            row_data);
    RETURN NULL;
END;
$$;

CREATE TRIGGER trg_platform_tenants_change AFTER INSERT OR UPDATE OR DELETE ON platform_tenants
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('TENANT', 'id');
CREATE TRIGGER trg_platform_applications_change AFTER INSERT OR UPDATE OR DELETE ON platform_applications
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('APPLICATION', 'id');
CREATE TRIGGER trg_platform_environments_change AFTER INSERT OR UPDATE OR DELETE ON platform_environments
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('ENVIRONMENT', 'id');
CREATE TRIGGER trg_platform_service_accounts_change AFTER INSERT OR UPDATE OR DELETE ON platform_service_accounts
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('SERVICE_ACCOUNT', 'id');
CREATE TRIGGER trg_platform_application_scopes_change AFTER INSERT OR UPDATE OR DELETE ON platform_application_scopes
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('APPLICATION_SCOPE', 'application_scope');
CREATE TRIGGER trg_platform_api_credentials_change AFTER INSERT OR UPDATE OR DELETE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('CREDENTIAL', 'id');
CREATE TRIGGER trg_platform_api_credential_scopes_change AFTER INSERT OR UPDATE OR DELETE ON platform_api_credential_scopes
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('CREDENTIAL_SCOPE', 'credential_scope');
CREATE TRIGGER trg_platform_principals_change AFTER INSERT OR UPDATE OR DELETE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('PRINCIPAL', 'id');
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_platform_principals_change ON platform_principals;
DROP TRIGGER IF EXISTS trg_platform_api_credential_scopes_change ON platform_api_credential_scopes;
DROP TRIGGER IF EXISTS trg_platform_api_credentials_change ON platform_api_credentials;
DROP TRIGGER IF EXISTS trg_platform_application_scopes_change ON platform_application_scopes;
DROP TRIGGER IF EXISTS trg_platform_service_accounts_change ON platform_service_accounts;
DROP TRIGGER IF EXISTS trg_platform_environments_change ON platform_environments;
DROP TRIGGER IF EXISTS trg_platform_applications_change ON platform_applications;
DROP TRIGGER IF EXISTS trg_platform_tenants_change ON platform_tenants;
DROP FUNCTION IF EXISTS platform_record_identity_change();
DROP TABLE IF EXISTS platform_identity_changes;
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
