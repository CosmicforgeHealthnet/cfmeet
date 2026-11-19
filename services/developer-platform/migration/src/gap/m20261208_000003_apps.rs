//! ADR 0064 / 0065 — CF Meet Apps (Developer Console part). Additive only.
//!
//! * `devplat_apps`: the app registered on a project (the project's Platform Access
//!   application id is the OAuth `client_id`): review status, which manifest version is
//!   current / submitted / reviewed / published, the draft test accounts (CF Meet account
//!   ids, at most 25) and the client secret's METADATA (id, issue time). The client secret
//!   itself is issued, verified and rotated by Platform Access and never stored here.
//! * `devplat_app_versions`: immutable manifest versions (append-only).
//! * `devplat_app_transitions`: the audited review history (append-only).
//! * `devplat_app_installations`: the projection of Platform Access `APP_GRANT` change-feed
//!   entries the install routing pass reads (grant id, app, target kind, install target
//!   scope, principal, OAuth scopes, status). No account id, no token.
//! * `devplat_feed_cursors`: the position of each change feed this service follows.
//!
//! Rollback: DOWN drops these tables (Platform Access keeps the OAuth clients and grants;
//! the projection is rebuilt from the feed).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_apps (
    app_id               UUID          NOT NULL,
    tenant_id            UUID          NOT NULL,
    status               VARCHAR(16)   NOT NULL,
    current_version      INTEGER       NOT NULL,
    submitted_version    INTEGER,
    reviewed_version     INTEGER,
    published_version    INTEGER,
    test_account_ids     UUID[]        NOT NULL DEFAULT '{}',
    secret_id            VARCHAR(64),
    secret_issued_at     TIMESTAMPTZ,
    reviewer_ref         VARCHAR(100),
    decision_reason_code VARCHAR(64),
    decided_at           TIMESTAMPTZ,
    published_at         TIMESTAMPTZ,
    synced_at            TIMESTAMPTZ,
    created_by           VARCHAR(100)  NOT NULL,
    created_at           TIMESTAMPTZ   NOT NULL,
    updated_at           TIMESTAMPTZ   NOT NULL,
    version              BIGINT        NOT NULL DEFAULT 1,
    CONSTRAINT pk_devplat_apps PRIMARY KEY (app_id),
    CONSTRAINT fk_devplat_apps_project FOREIGN KEY (app_id) REFERENCES devplat_project_profiles (application_id),
    CONSTRAINT fk_devplat_apps_org FOREIGN KEY (tenant_id) REFERENCES devplat_organizations (tenant_id),
    CONSTRAINT ck_devplat_apps_status CHECK (status IN
        ('DRAFT', 'SUBMITTED', 'IN_REVIEW', 'APPROVED', 'REJECTED', 'PUBLISHED', 'SUSPENDED')),
    CONSTRAINT ck_devplat_apps_versions CHECK (current_version >= 1
        AND (submitted_version IS NULL OR submitted_version BETWEEN 1 AND current_version)
        AND (reviewed_version IS NULL OR reviewed_version BETWEEN 1 AND current_version)
        AND (published_version IS NULL OR published_version BETWEEN 1 AND current_version)),
    CONSTRAINT ck_devplat_apps_test_accounts CHECK (cardinality(test_account_ids) <= 25),
    CONSTRAINT ck_devplat_apps_secret_id CHECK (secret_id IS NULL OR secret_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    CONSTRAINT ck_devplat_apps_reason CHECK (decision_reason_code IS NULL OR decision_reason_code ~ '^[A-Z][A-Z0-9_]{0,63}$')
);
CREATE INDEX ix_devplat_apps_tenant ON devplat_apps (tenant_id);
CREATE INDEX ix_devplat_apps_directory ON devplat_apps (published_at DESC, app_id DESC)
    WHERE published_version IS NOT NULL AND status <> 'SUSPENDED';

CREATE TABLE devplat_app_versions (
    app_id      UUID          NOT NULL,
    version     INTEGER       NOT NULL,
    manifest    JSONB         NOT NULL,
    created_by  VARCHAR(100)  NOT NULL,
    created_at  TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_app_versions PRIMARY KEY (app_id, version),
    CONSTRAINT fk_devplat_app_versions_app FOREIGN KEY (app_id) REFERENCES devplat_apps (app_id),
    CONSTRAINT ck_devplat_app_versions_version CHECK (version >= 1),
    CONSTRAINT ck_devplat_app_versions_manifest CHECK (
        jsonb_typeof(manifest) = 'object' AND octet_length(manifest::text) <= 32768)
);
CREATE TRIGGER tr_devplat_app_versions_append_only BEFORE UPDATE OR DELETE ON devplat_app_versions
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_app_versions_no_truncate BEFORE TRUNCATE ON devplat_app_versions
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_app_transitions (
    id           UUID          NOT NULL,
    app_id       UUID          NOT NULL,
    tenant_id    UUID          NOT NULL,
    from_status  VARCHAR(16),
    to_status    VARCHAR(16)   NOT NULL,
    app_version  INTEGER       NOT NULL,
    actor_kind   VARCHAR(16)   NOT NULL,
    actor_ref    VARCHAR(100)  NOT NULL,
    reason_code  VARCHAR(64),
    occurred_at  TIMESTAMPTZ   NOT NULL,
    seq          BIGSERIAL     NOT NULL,
    CONSTRAINT pk_devplat_app_transitions PRIMARY KEY (id),
    CONSTRAINT fk_devplat_app_transitions_app FOREIGN KEY (app_id) REFERENCES devplat_apps (app_id),
    CONSTRAINT ck_devplat_app_transitions_actor CHECK (actor_kind IN ('USER', 'OPERATOR', 'SERVICE')),
    CONSTRAINT ck_devplat_app_transitions_reason CHECK (reason_code IS NULL OR reason_code ~ '^[A-Z][A-Z0-9_]{0,63}$')
);
CREATE INDEX ix_devplat_app_transitions_app ON devplat_app_transitions (app_id, seq);
CREATE TRIGGER tr_devplat_app_transitions_append_only BEFORE UPDATE OR DELETE ON devplat_app_transitions
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_app_transitions_no_truncate BEFORE TRUNCATE ON devplat_app_transitions
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_app_installations (
    grant_id               UUID          NOT NULL,
    app_id                 UUID          NOT NULL,
    target_kind            VARCHAR(16)   NOT NULL,
    organization_id        UUID,
    principal_id           UUID,
    target_tenant_id       UUID,
    target_application_id  UUID,
    target_environment_id  UUID,
    scopes                 TEXT[]        NOT NULL DEFAULT '{}',
    status                 VARCHAR(16)   NOT NULL,
    revoked_reason         VARCHAR(64),
    revision               BIGINT        NOT NULL DEFAULT 0,
    feed_sequence          BIGINT        NOT NULL,
    installed_at           TIMESTAMPTZ   NOT NULL,
    updated_at             TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_devplat_app_installations PRIMARY KEY (grant_id),
    CONSTRAINT ck_devplat_app_installations_kind CHECK (target_kind IN ('USER', 'ORGANIZATION')),
    CONSTRAINT ck_devplat_app_installations_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_devplat_app_installations_reason CHECK (revoked_reason IS NULL OR revoked_reason ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_devplat_app_installations_scopes CHECK (cardinality(scopes) <= 32)
);
CREATE INDEX ix_devplat_app_installations_target ON devplat_app_installations
    (target_tenant_id, target_application_id, target_environment_id) WHERE status = 'ACTIVE';
CREATE INDEX ix_devplat_app_installations_app ON devplat_app_installations (app_id);

CREATE TABLE devplat_feed_cursors (
    feed            VARCHAR(64)  NOT NULL,
    after_sequence  BIGINT       NOT NULL,
    updated_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_feed_cursors PRIMARY KEY (feed),
    CONSTRAINT ck_devplat_feed_cursors_after CHECK (after_sequence >= 0)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_feed_cursors;
DROP TABLE IF EXISTS devplat_app_installations;
DROP TABLE IF EXISTS devplat_app_transitions;
DROP TABLE IF EXISTS devplat_app_versions;
DROP TABLE IF EXISTS devplat_apps;
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
