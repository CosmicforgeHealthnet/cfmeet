//! ADR 0068, phase A4 — API keys issued or rotated by the HQ admin bridge are delivered by
//! Platform Access itself, never through HQ. Additive only.
//!
//! * `platform_credential_deliveries`: one row per bridge-issued (or bridge-rotated) key.
//!   The new key's secret exists only as `sealed` (XChaCha20-Poly1305 under a key derived
//!   from the credential pepper of `key_version`, bound to the row and the credential)
//!   while the delivery is `PENDING`. The partner reveals it ONCE in their own Developer
//!   Platform console (delegation); the reveal, the expiry (`expires_at`, 7 days) and a
//!   revoked key all destroy the sealed copy in the same statement that leaves `PENDING`
//!   (`REVEALED` / `EXPIRED`): the CHECK makes a sealed copy outside `PENDING`
//!   impossible. Nothing is ever resent: an expired delivery is re-issued by a new
//!   command. The row itself (ids, channel, state, times) stays as the reference HQ reads.
//! * The admin bridge ledger gains the subjects `KEY_ISSUE` (subject: the service account;
//!   the new key is the record's `resulting_id`) and `KEY_ROTATION` (subject: the rotated
//!   key; the successor is the `resulting_id`).
//! * The operator audit vocabulary gains `CREDENTIAL_DELIVERY_REVEALED` (target kind
//!   `CREDENTIAL`): the partner's one reveal.
//!
//! Rollback: DOWN refuses to run once a delivery exists; otherwise it drops the table and
//! restores both vocabularies.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ACTIONS_V6: &str = "'TENANT_CREATED', 'TENANT_STATUS_SET',
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
        'ENVIRONMENT_ORIGINS_SET', 'USER_TOKEN_TEST_ISSUED', 'APP_GRANT_REVOKED'";

const SUBJECTS_V1: &str = "'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', 'CREDENTIAL',
        'PRINCIPAL', 'APP_GRANT', 'ENVIRONMENT_ORIGINS', 'APPLICATION_SCOPE',
        'PRINCIPAL_REGISTRATION', 'PRINCIPAL_SESSIONS'";

const UP: &str = r#"
CREATE TABLE platform_credential_deliveries (
    id              UUID         NOT NULL,
    credential_id   UUID         NOT NULL,
    tenant_id       UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    channel         VARCHAR(32)  NOT NULL,
    state           VARCHAR(16)  NOT NULL,
    sealed          BYTEA,
    nonce           BYTEA,
    key_version     INTEGER      NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    expires_at      TIMESTAMPTZ  NOT NULL,
    revealed_at     TIMESTAMPTZ,
    revealed_by     VARCHAR(100),
    closed_at       TIMESTAMPTZ,
    CONSTRAINT pk_platform_credential_deliveries PRIMARY KEY (id),
    CONSTRAINT uq_platform_credential_deliveries_credential UNIQUE (credential_id),
    CONSTRAINT fk_platform_credential_deliveries_credential
        FOREIGN KEY (credential_id) REFERENCES platform_api_credentials (id) ON DELETE CASCADE,
    CONSTRAINT ck_platform_credential_deliveries_channel CHECK (channel IN ('CONSOLE_REVEAL_ONCE')),
    CONSTRAINT ck_platform_credential_deliveries_state CHECK (state IN ('PENDING', 'REVEALED', 'EXPIRED')),
    CONSTRAINT ck_platform_credential_deliveries_sealed_only_pending CHECK (
        (state = 'PENDING' AND sealed IS NOT NULL AND nonce IS NOT NULL AND closed_at IS NULL)
        OR (state <> 'PENDING' AND sealed IS NULL AND nonce IS NULL AND closed_at IS NOT NULL)),
    CONSTRAINT ck_platform_credential_deliveries_revealed CHECK (
        (state = 'REVEALED') = (revealed_at IS NOT NULL AND revealed_by IS NOT NULL)),
    CONSTRAINT ck_platform_credential_deliveries_window CHECK (expires_at > created_at),
    CONSTRAINT ck_platform_credential_deliveries_key_version CHECK (key_version > 0),
    CONSTRAINT ck_platform_credential_deliveries_sealed_size CHECK (
        sealed IS NULL OR octet_length(sealed) <= 512)
);
CREATE INDEX idx_platform_credential_deliveries_pending
    ON platform_credential_deliveries (expires_at) WHERE state = 'PENDING';
CREATE INDEX idx_platform_credential_deliveries_environment
    ON platform_credential_deliveries (environment_id, state);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(UP).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_admin_operations \
               DROP CONSTRAINT ck_platform_access_admin_operations_subject; \
             ALTER TABLE platform_access_admin_operations \
               ADD CONSTRAINT ck_platform_access_admin_operations_subject \
               CHECK (subject_kind IN ({SUBJECTS_V1}, 'KEY_ISSUE', 'KEY_ROTATION')); \
             ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
               CHECK (action IN ({ACTIONS_V6}, 'CREDENTIAL_DELIVERY_REVEALED'));"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_credential_deliveries) \
                  OR EXISTS (SELECT 1 FROM platform_access_admin_operations \
                             WHERE subject_kind IN ('KEY_ISSUE', 'KEY_ROTATION')) \
                  OR EXISTS (SELECT 1 FROM platform_access_audit \
                             WHERE action = 'CREDENTIAL_DELIVERY_REVEALED') THEN \
                 RAISE EXCEPTION 'owner-delivered API keys were recorded; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
               CHECK (action IN ({ACTIONS_V6})); \
             ALTER TABLE platform_access_admin_operations \
               DROP CONSTRAINT ck_platform_access_admin_operations_subject; \
             ALTER TABLE platform_access_admin_operations \
               ADD CONSTRAINT ck_platform_access_admin_operations_subject \
               CHECK (subject_kind IN ({SUBJECTS_V1}));"
        ))
        .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS platform_credential_deliveries;")
            .await?;
        Ok(())
    }
}
