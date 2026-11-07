//! Blueprint v1.0 Section 15 (PLT-008, ORG-013) — credential last-use and tenant rename.
//!
//! * `platform_credential_usage`: when a credential last authenticated. Kept OUT of
//!   `platform_api_credentials` on purpose: an update of the credential row bumps its
//!   revision and writes an identity change-feed row, so recording use there would turn
//!   every authentication into a cache invalidation for every consumer. This table has no
//!   revision, no change-feed trigger, and is written at most once per credential per
//!   debounce window (`CredentialAuthenticator`), never on every request. One row per
//!   credential; the row follows the credential (`ON DELETE CASCADE`, credentials are
//!   never deleted in practice).
//! * The operator audit vocabulary gains `TENANT_RENAMED` (the Developer Platform
//!   propagates an organization rename to its EXTERNAL tenant through delegation).
//!
//! Additive and rollback-safe: the down migration refuses to run once a rename has been
//! audited (the old CHECK could not hold), and otherwise drops the usage table (derived,
//! non-authoritative data).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ACTIONS_V3: &str = "'TENANT_CREATED', 'TENANT_STATUS_SET',
        'APPLICATION_CREATED', 'APPLICATION_STATUS_SET',
        'ENVIRONMENT_CREATED', 'ENVIRONMENT_STATUS_SET', 'ENVIRONMENT_LIVE_TELECOM_SET',
        'SERVICE_ACCOUNT_CREATED', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'CREDENTIAL_PRINCIPALS_RESTRICTED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'PRIVILEGED_SCOPE_GRANTED',
        'PRINCIPAL_REGISTERED', 'PRINCIPAL_STATUS_SET', 'PRINCIPAL_TOMBSTONED',
        'IDENTITY_IMPORTED', 'CREDENTIAL_SUSPENDED', 'CREDENTIAL_RESUMED',
        'PRODUCTION_ENVIRONMENT_ACTIVATED', 'ORGANIZATION_SCOPE_PROVISIONED'";

const UP: &str = r#"
CREATE TABLE platform_credential_usage (
    credential_id          UUID         NOT NULL,
    last_authenticated_at  TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_credential_usage PRIMARY KEY (credential_id),
    CONSTRAINT fk_platform_credential_usage_credential
        FOREIGN KEY (credential_id) REFERENCES platform_api_credentials (id) ON DELETE CASCADE
);
CREATE INDEX idx_platform_access_audit_recent ON platform_access_audit (occurred_at DESC, id DESC);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(UP).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V3}, 'TENANT_RENAMED'));"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_access_audit WHERE action = 'TENANT_RENAMED') THEN \
                 RAISE EXCEPTION 'Section 15 tenant renames are audited; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V3})); \
             DROP INDEX IF EXISTS idx_platform_access_audit_recent; \
             DROP TABLE IF EXISTS platform_credential_usage;"
        ))
        .await?;
        Ok(())
    }
}
