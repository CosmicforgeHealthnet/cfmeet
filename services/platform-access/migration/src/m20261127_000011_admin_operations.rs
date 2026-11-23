//! ADR 0068, phase A4 — identity commands relayed by the HQ admin bridge. Additive only.
//!
//! * `platform_access_admin_operations` records every bridge command this service
//!   APPLIED, in the same transaction as the change, keyed by HQ's `Idempotency-Key`: the
//!   command (capability id), its subject (kind, id and, for a scope grant or an allowed
//!   website, the scope name / origin), who acted (`hq:<staff>`), who approved it, and the
//!   resulting state and version. The bridge settles an unknown outcome from this record
//!   (`GET /internal/v1/operator/operations/{key}`); a key that is not here was never
//!   applied. Append-only.
//! * `platform_credential_suspension_changes`: how many times a credential was suspended
//!   or resumed. A suspension lives in its own table (the credential snapshot is a
//!   published contract), so it never changed the credential's `revision`; the bridge's
//!   expected version of a credential is `revision + changes`, which moves on EVERY
//!   effective change (revoke, rotate, suspend, resume).
//! * The operator audit vocabulary gains `APP_GRANT_REVOKED` (target kind `APP_GRANT`):
//!   a staff revocation of a CF Meet Apps grant.
//!
//! Rollback: DOWN refuses to run once a bridge command or an app-grant revocation was
//! recorded; otherwise it drops both tables and restores the vocabulary.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ACTIONS_V5: &str = "'TENANT_CREATED', 'TENANT_STATUS_SET',
        'APPLICATION_CREATED', 'APPLICATION_STATUS_SET',
        'ENVIRONMENT_CREATED', 'ENVIRONMENT_STATUS_SET', 'ENVIRONMENT_LIVE_TELECOM_SET',
        'SERVICE_ACCOUNT_CREATED', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'CREDENTIAL_PRINCIPALS_RESTRICTED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'PRIVILEGED_SCOPE_GRANTED',
        'PRINCIPAL_REGISTERED', 'PRINCIPAL_STATUS_SET', 'PRINCIPAL_TOMBSTONED',
        'IDENTITY_IMPORTED', 'CREDENTIAL_SUSPENDED', 'CREDENTIAL_RESUMED',
        'PRODUCTION_ENVIRONMENT_ACTIVATED', 'ORGANIZATION_SCOPE_PROVISIONED',
        'TENANT_RENAMED', 'PRINCIPAL_PROFILE_SET', 'PRINCIPAL_SESSIONS_REVOKED',
        'ENVIRONMENT_ORIGINS_SET', 'USER_TOKEN_TEST_ISSUED'";

const TARGET_KINDS_V1: &str = "'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT',
        'CREDENTIAL', 'PRINCIPAL', 'IDENTITY_IMPORT'";

const UP: &str = r#"
CREATE TABLE platform_access_admin_operations (
    idempotency_key   UUID          NOT NULL,
    command           VARCHAR(64)   NOT NULL,
    subject_kind      VARCHAR(32)   NOT NULL,
    subject_id        UUID          NOT NULL,
    subject_detail    VARCHAR(300),
    tenant_id         UUID          NOT NULL,
    resulting_id      UUID,
    operator_ref      VARCHAR(160)  NOT NULL,
    approver_ref      VARCHAR(160),
    approval_ref      VARCHAR(128),
    resulting_state   VARCHAR(32)   NOT NULL,
    resulting_version BIGINT,
    recorded_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_platform_access_admin_operations PRIMARY KEY (idempotency_key),
    CONSTRAINT ck_platform_access_admin_operations_command CHECK (command ~ '^[a-z][a-z0-9.-]{1,63}$'),
    CONSTRAINT ck_platform_access_admin_operations_subject CHECK (subject_kind IN (
        'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL', 'PRINCIPAL',
        'APP_GRANT', 'ENVIRONMENT_ORIGINS', 'APPLICATION_SCOPE', 'PRINCIPAL_REGISTRATION',
        'PRINCIPAL_SESSIONS')),
    CONSTRAINT ck_platform_access_admin_operations_operator CHECK (operator_ref ~ '^hq:[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_platform_access_admin_operations_approver CHECK
        ((approver_ref IS NULL) = (approval_ref IS NULL)
         AND (approver_ref IS NULL OR (approver_ref ~ '^hq:' AND approver_ref <> operator_ref))),
    CONSTRAINT ck_platform_access_admin_operations_state CHECK (resulting_state ~ '^[A-Z][A-Z_]{1,31}$'),
    CONSTRAINT ck_platform_access_admin_operations_version CHECK (resulting_version IS NULL OR resulting_version > 0)
);
CREATE INDEX idx_platform_access_admin_operations_subject
    ON platform_access_admin_operations (subject_kind, subject_id, recorded_at);
CREATE TRIGGER trg_platform_access_admin_operations_append_only
    BEFORE UPDATE OR DELETE ON platform_access_admin_operations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
CREATE TRIGGER trg_platform_access_admin_operations_no_truncate
    BEFORE TRUNCATE ON platform_access_admin_operations
    FOR EACH STATEMENT EXECUTE FUNCTION platform_forbid_mutation();

CREATE TABLE platform_credential_suspension_changes (
    credential_id UUID   NOT NULL,
    changes       BIGINT NOT NULL,
    CONSTRAINT pk_platform_credential_suspension_changes PRIMARY KEY (credential_id),
    CONSTRAINT fk_platform_credential_suspension_changes_credential
        FOREIGN KEY (credential_id) REFERENCES platform_api_credentials (id) ON DELETE CASCADE,
    CONSTRAINT ck_platform_credential_suspension_changes_changes CHECK (changes > 0)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_credential_suspension_changes;
DROP TABLE IF EXISTS platform_access_admin_operations;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(UP).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V5}, 'APP_GRANT_REVOKED')); \
             ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_target_kind; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_target_kind \
             CHECK (target_kind IN ({TARGET_KINDS_V1}, 'APP_GRANT'));"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_access_admin_operations) \
                  OR EXISTS (SELECT 1 FROM platform_access_audit \
                             WHERE action = 'APP_GRANT_REVOKED' OR target_kind = 'APP_GRANT') THEN \
                 RAISE EXCEPTION 'admin bridge commands were recorded; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_target_kind; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_target_kind \
             CHECK (target_kind IN ({TARGET_KINDS_V1})); \
             ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V5}));"
        ))
        .await?;
        db.execute_unprepared(DOWN).await?;
        Ok(())
    }
}
