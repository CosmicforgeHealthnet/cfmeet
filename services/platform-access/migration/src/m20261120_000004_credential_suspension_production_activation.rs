//! Part VI §54 / §55 — credential suspension and production-environment activation.
//!
//! * `platform_credential_suspensions`: a credential may be SUSPENDED (reversible) without
//!   changing its persisted `ACTIVE` / `REVOKED` status, so every existing credential
//!   snapshot consumer keeps its contract. Authentication, delegated-authority
//!   re-verification and credential status reads refuse a suspended credential
//!   (`credential_suspended`). Suspending / resuming also touches the credential row, so the
//!   identity change feed signals the change to caching consumers.
//! * `platform_production_activations`: the record that a PRODUCTION environment was
//!   activated by the Developer Platform's production-access approval workflow (the
//!   approval reference). Only such an environment accepts delegated service-account,
//!   credential, status and live-telecom changes; an operator-created PRODUCTION
//!   environment stays operator-only.
//! * The operator audit vocabulary gains the new actions.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ACTIONS_V1: &str = "'TENANT_CREATED', 'TENANT_STATUS_SET',
        'APPLICATION_CREATED', 'APPLICATION_STATUS_SET',
        'ENVIRONMENT_CREATED', 'ENVIRONMENT_STATUS_SET', 'ENVIRONMENT_LIVE_TELECOM_SET',
        'SERVICE_ACCOUNT_CREATED', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'CREDENTIAL_PRINCIPALS_RESTRICTED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'PRIVILEGED_SCOPE_GRANTED',
        'PRINCIPAL_REGISTERED', 'PRINCIPAL_STATUS_SET', 'PRINCIPAL_TOMBSTONED',
        'IDENTITY_IMPORTED'";

const UP: &str = r#"
CREATE TABLE platform_credential_suspensions (
    credential_id  UUID         NOT NULL,
    tenant_id      UUID         NOT NULL,
    suspended_at   TIMESTAMPTZ  NOT NULL,
    suspended_by   VARCHAR(100) NOT NULL,
    reason_code    VARCHAR(64)  NOT NULL,
    CONSTRAINT pk_platform_credential_suspensions PRIMARY KEY (credential_id),
    CONSTRAINT fk_platform_credential_suspensions_credential
        FOREIGN KEY (credential_id) REFERENCES platform_api_credentials (id) ON DELETE CASCADE,
    CONSTRAINT ck_platform_credential_suspensions_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$')
);

CREATE TABLE platform_production_activations (
    environment_id     UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    approval_reference VARCHAR(128) NOT NULL,
    activated_by       VARCHAR(100) NOT NULL,
    activated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_production_activations PRIMARY KEY (environment_id),
    CONSTRAINT uq_platform_production_activations_application UNIQUE (application_id),
    CONSTRAINT fk_platform_production_activations_environment
        FOREIGN KEY (environment_id) REFERENCES platform_environments (id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_production_activations_reference
        CHECK (approval_reference ~ '^[A-Za-z0-9][A-Za-z0-9_:-]{0,127}$')
);
CREATE TRIGGER trg_platform_production_activations_append_only
    BEFORE UPDATE OR DELETE ON platform_production_activations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(UP).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V1}, 'CREDENTIAL_SUSPENDED', 'CREDENTIAL_RESUMED', \
             'PRODUCTION_ENVIRONMENT_ACTIVATED'));"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_access_audit WHERE action IN \
                 ('CREDENTIAL_SUSPENDED', 'CREDENTIAL_RESUMED', 'PRODUCTION_ENVIRONMENT_ACTIVATED')) THEN \
                 RAISE EXCEPTION 'Part VI audit rows exist; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V1})); \
             DROP TABLE platform_production_activations; \
             DROP TABLE platform_credential_suspensions;"
        ))
        .await?;
        Ok(())
    }
}
