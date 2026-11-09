//! Blueprint v1.0 Section 7 (ADR 0058) — the governed FIRST_PARTY organization scopes.
//!
//! * `platform_organization_scopes`: the one dedicated FIRST_PARTY tenant + first-party
//!   application + PRODUCTION-kind environment Platform Access created for one first-party
//!   business organization of the Organizations Service. The organization id is an
//!   OPAQUE reference (`organization_ref`): Platform Access never reads, owns or interprets
//!   the organization. One row per `(organization_ref, capability)` is the idempotency key
//!   of the provisioning operation; a tenant backs at most one organization scope and the
//!   composite foreign key pins tenant ⊃ application ⊃ environment. Rows are immutable
//!   (append-only): a scope is suspended / resumed through its tenant's status, never
//!   re-pointed.
//! * The operator audit vocabulary gains `ORGANIZATION_SCOPE_PROVISIONED`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ACTIONS_V2: &str = "'TENANT_CREATED', 'TENANT_STATUS_SET',
        'APPLICATION_CREATED', 'APPLICATION_STATUS_SET',
        'ENVIRONMENT_CREATED', 'ENVIRONMENT_STATUS_SET', 'ENVIRONMENT_LIVE_TELECOM_SET',
        'SERVICE_ACCOUNT_CREATED', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'CREDENTIAL_PRINCIPALS_RESTRICTED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'PRIVILEGED_SCOPE_GRANTED',
        'PRINCIPAL_REGISTERED', 'PRINCIPAL_STATUS_SET', 'PRINCIPAL_TOMBSTONED',
        'IDENTITY_IMPORTED', 'CREDENTIAL_SUSPENDED', 'CREDENTIAL_RESUMED',
        'PRODUCTION_ENVIRONMENT_ACTIVATED'";

const UP: &str = r#"
CREATE TABLE platform_organization_scopes (
    organization_ref UUID         NOT NULL,
    capability       VARCHAR(32)  NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    requested_by     VARCHAR(64)  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_organization_scopes PRIMARY KEY (organization_ref, capability),
    CONSTRAINT uq_platform_organization_scopes_tenant UNIQUE (tenant_id),
    CONSTRAINT fk_platform_organization_scopes_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_organization_scopes_capability CHECK (capability ~ '^[a-z][a-z0-9-]{0,31}$'),
    CONSTRAINT ck_platform_organization_scopes_requester CHECK (requested_by ~ '^cfmeet\.[a-z][a-z0-9-]*$')
);
CREATE TRIGGER trg_platform_organization_scopes_append_only
    BEFORE UPDATE OR DELETE ON platform_organization_scopes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
CREATE TRIGGER trg_platform_organization_scopes_no_truncate
    BEFORE TRUNCATE ON platform_organization_scopes
    FOR EACH STATEMENT EXECUTE FUNCTION platform_forbid_mutation();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(UP).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V2}, 'ORGANIZATION_SCOPE_PROVISIONED'));"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_access_audit WHERE action = 'ORGANIZATION_SCOPE_PROVISIONED') \
                  OR EXISTS (SELECT 1 FROM platform_organization_scopes) THEN \
                 RAISE EXCEPTION 'Section 7 organization scopes exist; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V2})); \
             DROP TABLE platform_organization_scopes;"
        ))
        .await?;
        Ok(())
    }
}
