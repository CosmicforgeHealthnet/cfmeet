//! ADR 0062 (CF Meet Platform "Build") — partner users, user tokens, allowed websites.
//!
//! Every table here is read and written only behind default-off feature flags
//! (`PLATFORM_ACCESS_END_USERS_ENABLED`, `PLATFORM_ACCESS_USER_TOKENS_ENABLED`,
//! `PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED`); with the flags off nothing writes them.
//!
//! * `platform_end_user_profiles`: the profile of an EXTERNAL_APPLICATION_PRINCIPAL
//!   partner user (display name, image URL) and its session epoch `tokens_not_before`
//!   (`revoke-sessions`: user tokens issued before it are dead). Kept OUT of
//!   `platform_principals` on purpose: a principal row change bumps its revision and
//!   writes an identity change-feed row whose snapshot shape is a published contract
//!   (`PrincipalSnapshot`, no unknown fields), and profiles are partner data, not identity.
//!   The row is pinned to the principal's own scope by a composite foreign key. A DELETE
//!   (tombstone) erases the profile values and keeps the row (and the epoch).
//! * `platform_user_token_mint_windows`: bounded per-user mint counters (one row per user
//!   and clock hour; unlogged, pruned periodically) for the 120-mints-per-hour limit.
//! * `platform_environment_origins`: the allowed websites (exact origins, at most 20) of an
//!   environment, and `platform_environment_origin_changes`, the append-only change feed
//!   consumers (API Gateway, Messaging, Calling, Meeting Service) follow. A feed row is
//!   written on every origin change and, by trigger, on every status change of an
//!   environment that has origins (or of its application / tenant), under a
//!   transaction-scoped advisory lock so a committed sequence is never overtaken.
//! * The operator audit vocabulary gains `USER_TOKEN_TEST_ISSUED`, `PRINCIPAL_PROFILE_SET`,
//!   `PRINCIPAL_SESSIONS_REVOKED` and `ENVIRONMENT_ORIGINS_SET`.
//! * An index for listing an environment's principals of one kind in creation order.
//!
//! Additive. The down migration refuses to run once any of the new data exists.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ACTIONS_V4: &str = "'TENANT_CREATED', 'TENANT_STATUS_SET',
        'APPLICATION_CREATED', 'APPLICATION_STATUS_SET',
        'ENVIRONMENT_CREATED', 'ENVIRONMENT_STATUS_SET', 'ENVIRONMENT_LIVE_TELECOM_SET',
        'SERVICE_ACCOUNT_CREATED', 'SERVICE_ACCOUNT_STATUS_SET',
        'CREDENTIAL_CREATED', 'CREDENTIAL_ROTATED', 'CREDENTIAL_REVOKED',
        'CREDENTIAL_PRINCIPALS_RESTRICTED',
        'SCOPE_GRANTED', 'SCOPE_REVOKED', 'PRIVILEGED_SCOPE_GRANTED',
        'PRINCIPAL_REGISTERED', 'PRINCIPAL_STATUS_SET', 'PRINCIPAL_TOMBSTONED',
        'IDENTITY_IMPORTED', 'CREDENTIAL_SUSPENDED', 'CREDENTIAL_RESUMED',
        'PRODUCTION_ENVIRONMENT_ACTIVATED', 'ORGANIZATION_SCOPE_PROVISIONED',
        'TENANT_RENAMED'";

const UP: &str = r#"
CREATE TABLE platform_end_user_profiles (
    principal_id       UUID         NOT NULL,
    tenant_id          UUID         NOT NULL,
    application_id     UUID         NOT NULL,
    environment_id     UUID         NOT NULL,
    display_name       VARCHAR(64),
    image_url          VARCHAR(512),
    tokens_not_before  TIMESTAMPTZ,
    created_at         TIMESTAMPTZ  NOT NULL,
    updated_at         TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_end_user_profiles PRIMARY KEY (principal_id),
    CONSTRAINT fk_platform_end_user_profiles_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_end_user_profiles_display_name
        CHECK (display_name IS NULL OR (char_length(display_name) BETWEEN 1 AND 64 AND display_name !~ '[[:cntrl:]]')),
    CONSTRAINT ck_platform_end_user_profiles_image_url
        CHECK (image_url IS NULL OR (image_url LIKE 'https://%' AND image_url !~ '[[:space:][:cntrl:]]'))
);
CREATE TRIGGER trg_platform_end_user_profiles_scope
    BEFORE UPDATE ON platform_end_user_profiles
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('principal_id', 'tenant_id', 'application_id', 'environment_id', 'created_at');

CREATE INDEX idx_platform_principals_environment_kind_created
    ON platform_principals (environment_id, kind, created_at, id);

CREATE UNLOGGED TABLE platform_user_token_mint_windows (
    principal_id UUID        NOT NULL,
    window_start TIMESTAMPTZ NOT NULL,
    mints        BIGINT      NOT NULL,
    CONSTRAINT pk_platform_user_token_mint_windows PRIMARY KEY (principal_id, window_start),
    CONSTRAINT ck_platform_user_token_mint_windows_mints CHECK (mints > 0)
);
CREATE INDEX idx_platform_user_token_mint_windows_start ON platform_user_token_mint_windows (window_start);

CREATE TABLE platform_environment_origins (
    environment_id UUID         NOT NULL,
    origins        TEXT[]       NOT NULL DEFAULT '{}',
    revision       BIGINT       NOT NULL DEFAULT 1,
    created_at     TIMESTAMPTZ  NOT NULL,
    updated_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_environment_origins PRIMARY KEY (environment_id),
    CONSTRAINT fk_platform_environment_origins_environment
        FOREIGN KEY (environment_id) REFERENCES platform_environments (id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_environment_origins_count CHECK (cardinality(origins) <= 20),
    CONSTRAINT ck_platform_environment_origins_revision CHECK (revision > 0)
);
CREATE TRIGGER trg_platform_environment_origins_revision
    BEFORE UPDATE ON platform_environment_origins
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

CREATE TABLE platform_environment_origin_changes (
    seq            BIGSERIAL    PRIMARY KEY,
    environment_id UUID         NOT NULL,
    occurred_at    TIMESTAMPTZ  NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_platform_environment_origin_changes_environment
        FOREIGN KEY (environment_id) REFERENCES platform_environments (id) ON DELETE RESTRICT
);
CREATE INDEX idx_platform_environment_origin_changes_environment
    ON platform_environment_origin_changes (environment_id, seq);
CREATE TRIGGER trg_platform_environment_origin_changes_append_only
    BEFORE UPDATE OR DELETE ON platform_environment_origin_changes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();
CREATE TRIGGER trg_platform_environment_origin_changes_no_truncate
    BEFORE TRUNCATE ON platform_environment_origin_changes
    FOR EACH STATEMENT EXECUTE FUNCTION platform_forbid_mutation();

-- A status change of an environment with origins, or of the application / tenant above
-- it, changes the effective status consumers read from the feed. TG_ARGV[0] names the
-- level ('environment' | 'application' | 'tenant').
CREATE FUNCTION platform_record_origin_status_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status IS NOT DISTINCT FROM OLD.status THEN
        RETURN NULL;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM platform_environment_origins o
          JOIN platform_environments e ON e.id = o.environment_id
         WHERE (TG_ARGV[0] = 'environment' AND e.id = NEW.id)
            OR (TG_ARGV[0] = 'application' AND e.application_id = NEW.id)
            OR (TG_ARGV[0] = 'tenant' AND e.tenant_id = NEW.id)
    ) THEN
        RETURN NULL;
    END IF;
    -- Commit order == sequence order (same rule as the identity change feed).
    PERFORM pg_advisory_xact_lock(7239062);
    INSERT INTO platform_environment_origin_changes (environment_id)
    SELECT o.environment_id
      FROM platform_environment_origins o
      JOIN platform_environments e ON e.id = o.environment_id
     WHERE (TG_ARGV[0] = 'environment' AND e.id = NEW.id)
        OR (TG_ARGV[0] = 'application' AND e.application_id = NEW.id)
        OR (TG_ARGV[0] = 'tenant' AND e.tenant_id = NEW.id)
     ORDER BY o.environment_id;
    RETURN NULL;
END;
$$;
CREATE TRIGGER trg_platform_environments_origin_status AFTER UPDATE OF status ON platform_environments
    FOR EACH ROW EXECUTE FUNCTION platform_record_origin_status_change('environment');
CREATE TRIGGER trg_platform_applications_origin_status AFTER UPDATE OF status ON platform_applications
    FOR EACH ROW EXECUTE FUNCTION platform_record_origin_status_change('application');
CREATE TRIGGER trg_platform_tenants_origin_status AFTER UPDATE OF status ON platform_tenants
    FOR EACH ROW EXECUTE FUNCTION platform_record_origin_status_change('tenant');
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_platform_tenants_origin_status ON platform_tenants;
DROP TRIGGER IF EXISTS trg_platform_applications_origin_status ON platform_applications;
DROP TRIGGER IF EXISTS trg_platform_environments_origin_status ON platform_environments;
DROP FUNCTION IF EXISTS platform_record_origin_status_change();
DROP TABLE IF EXISTS platform_environment_origin_changes;
DROP TABLE IF EXISTS platform_environment_origins;
DROP TABLE IF EXISTS platform_user_token_mint_windows;
DROP INDEX IF EXISTS idx_platform_principals_environment_kind_created;
DROP TABLE IF EXISTS platform_end_user_profiles;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(UP).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V4}, 'PRINCIPAL_PROFILE_SET', 'PRINCIPAL_SESSIONS_REVOKED', \
             'ENVIRONMENT_ORIGINS_SET', 'USER_TOKEN_TEST_ISSUED'));"
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_end_user_profiles) \
                  OR EXISTS (SELECT 1 FROM platform_environment_origins) \
                  OR EXISTS (SELECT 1 FROM platform_environment_origin_changes) \
                  OR EXISTS (SELECT 1 FROM platform_access_audit WHERE action IN \
                     ('PRINCIPAL_PROFILE_SET', 'PRINCIPAL_SESSIONS_REVOKED', 'ENVIRONMENT_ORIGINS_SET', \
                      'USER_TOKEN_TEST_ISSUED')) THEN \
                 RAISE EXCEPTION 'partner users / allowed websites exist; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_access_audit DROP CONSTRAINT ck_platform_access_audit_action; \
             ALTER TABLE platform_access_audit ADD CONSTRAINT ck_platform_access_audit_action \
             CHECK (action IN ({ACTIONS_V4}));"
        ))
        .await?;
        db.execute_unprepared(DOWN).await?;
        Ok(())
    }
}
