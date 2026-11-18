//! ADR 0064 (CF Meet Apps, "Integrate") — OAuth clients, grants (installations), codes and
//! tokens. Every table here is read and written only behind the default-off flag
//! `PLATFORM_ACCESS_OAUTH_ENABLED`; with it off nothing writes them.
//!
//! * `platform_oauth_clients`: the OAuth client profile of an application (an app's
//!   `client_id` is the application id): exact redirect URIs, requested user / bot scopes,
//!   distribution, status, DRAFT test accounts and the PRIVATE allow lists the Developer
//!   Console syncs (Platform Access does not know workspace membership).
//! * `platform_oauth_client_secrets`: verifier-only client secrets (`cfcs_`); a rotated
//!   secret keeps working until `valid_until` (24 h overlap).
//! * `platform_oauth_grants`: one installation of an app for a CF Meet account (USER) or
//!   business organization (ORGANIZATION), pinned to its target FIRST_PARTY triple, its
//!   restricted principal and its installation service account by composite foreign keys.
//!   At most one ACTIVE grant per (app, account) / (app, organization); REVOKED is
//!   terminal. Every change is published on the identity change feed as `APP_GRANT`.
//! * `platform_oauth_codes`: single-use authorization codes (60 s), verifier-only, bound to
//!   the client, redirect URI, PKCE S256 challenge and grant.
//! * `platform_oauth_tokens`: which `platform_api_credentials` rows are OAuth access
//!   (`cfoa_`) or bot (`cfob_`) tokens of which grant. The tokens themselves are ordinary
//!   credentials of the installation service account (verifier-only, verified live by the
//!   same chain, principal-restricted by `platform_credential_principal_restrictions`).
//! * `platform_oauth_refresh_tokens`: rotating single-use refresh tokens (`cfor_`, 90 d),
//!   verifier-only; never platform credentials.
//!
//! Additive (the change-feed kind CHECK only gains `APP_GRANT`). The down migration refuses
//! to run once any of the new data exists.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const KINDS_V1: &str = "'TENANT', 'APPLICATION', 'ENVIRONMENT', 'SERVICE_ACCOUNT', \
    'APPLICATION_SCOPE', 'CREDENTIAL', 'CREDENTIAL_SCOPE', 'PRINCIPAL'";

const UP: &str = r#"
CREATE TABLE platform_oauth_clients (
    application_id           UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    name                     VARCHAR(200) NOT NULL,
    redirect_uris            TEXT[]       NOT NULL,
    user_scopes              TEXT[]       NOT NULL DEFAULT '{}',
    bot_scopes               TEXT[]       NOT NULL DEFAULT '{}',
    distribution             TEXT         NOT NULL,
    status                   TEXT         NOT NULL,
    test_account_ids         UUID[]       NOT NULL DEFAULT '{}',
    allowed_account_ids      UUID[]       NOT NULL DEFAULT '{}',
    allowed_organization_ids UUID[]       NOT NULL DEFAULT '{}',
    revision                 BIGINT       NOT NULL DEFAULT 1,
    created_at               TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_oauth_clients PRIMARY KEY (application_id),
    CONSTRAINT fk_platform_oauth_clients_application
        FOREIGN KEY (tenant_id, application_id)
        REFERENCES platform_applications (tenant_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_oauth_clients_name CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    CONSTRAINT ck_platform_oauth_clients_redirect_uris CHECK (cardinality(redirect_uris) BETWEEN 1 AND 10),
    CONSTRAINT ck_platform_oauth_clients_distribution CHECK (distribution IN ('PRIVATE', 'PUBLIC')),
    CONSTRAINT ck_platform_oauth_clients_status CHECK (status IN ('DRAFT', 'APPROVED', 'PUBLISHED', 'SUSPENDED')),
    CONSTRAINT ck_platform_oauth_clients_test_accounts CHECK (cardinality(test_account_ids) <= 25),
    CONSTRAINT ck_platform_oauth_clients_allowed_accounts CHECK (cardinality(allowed_account_ids) <= 1000),
    CONSTRAINT ck_platform_oauth_clients_allowed_organizations CHECK (cardinality(allowed_organization_ids) <= 1000),
    CONSTRAINT ck_platform_oauth_clients_revision CHECK (revision > 0)
);
CREATE TRIGGER trg_platform_oauth_clients_immutable
    BEFORE UPDATE ON platform_oauth_clients
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('application_id', 'tenant_id', 'created_at');
CREATE TRIGGER trg_platform_oauth_clients_revision
    BEFORE UPDATE ON platform_oauth_clients
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();

CREATE TABLE platform_oauth_client_secrets (
    id                   UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    public_id            VARCHAR(32)  NOT NULL,
    environment_tag      VARCHAR(8)   NOT NULL,
    verifier             BYTEA        NOT NULL,
    verifier_key_version INTEGER      NOT NULL,
    last_four            CHAR(4)      NOT NULL,
    issued_at            TIMESTAMPTZ  NOT NULL,
    valid_until          TIMESTAMPTZ,
    revoked_at           TIMESTAMPTZ,
    CONSTRAINT pk_platform_oauth_client_secrets PRIMARY KEY (id),
    CONSTRAINT fk_platform_oauth_client_secrets_client
        FOREIGN KEY (application_id) REFERENCES platform_oauth_clients (application_id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_oauth_client_secrets_public_id UNIQUE (public_id),
    CONSTRAINT ck_platform_oauth_client_secrets_public_id CHECK (public_id ~ '^[0-9a-f]{24}$'),
    CONSTRAINT ck_platform_oauth_client_secrets_tag CHECK (environment_tag IN ('sbx', 'test', 'live')),
    CONSTRAINT ck_platform_oauth_client_secrets_verifier CHECK (octet_length(verifier) = 32),
    CONSTRAINT ck_platform_oauth_client_secrets_key_version CHECK (verifier_key_version > 0),
    CONSTRAINT ck_platform_oauth_client_secrets_last_four CHECK (last_four ~ '^[0-9a-f]{4}$')
);
CREATE INDEX idx_platform_oauth_client_secrets_application ON platform_oauth_client_secrets (application_id, issued_at);
CREATE TRIGGER trg_platform_oauth_client_secrets_immutable
    BEFORE UPDATE ON platform_oauth_client_secrets
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'application_id', 'public_id', 'environment_tag', 'verifier', 'verifier_key_version',
        'last_four', 'issued_at');

CREATE TABLE platform_oauth_grants (
    id                      UUID         NOT NULL,
    app_id                  UUID         NOT NULL,
    target                  TEXT         NOT NULL,
    account_id              UUID,
    organization_id         UUID,
    installed_by_account_id UUID         NOT NULL,
    tenant_id               UUID         NOT NULL,
    application_id          UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    principal_id            UUID         NOT NULL,
    service_account_id      UUID         NOT NULL,
    scopes                  TEXT[]       NOT NULL,
    status                  TEXT         NOT NULL DEFAULT 'ACTIVE',
    revoked_at              TIMESTAMPTZ,
    revoke_reason           VARCHAR(32),
    revision                BIGINT       NOT NULL DEFAULT 1,
    created_at              TIMESTAMPTZ  NOT NULL,
    updated_at              TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_oauth_grants PRIMARY KEY (id),
    CONSTRAINT fk_platform_oauth_grants_client
        FOREIGN KEY (app_id) REFERENCES platform_oauth_clients (application_id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_oauth_grants_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_oauth_grants_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_oauth_grants_target CHECK (
        (target = 'USER' AND account_id IS NOT NULL AND organization_id IS NULL)
        OR (target = 'ORGANIZATION' AND organization_id IS NOT NULL AND account_id IS NULL)),
    CONSTRAINT ck_platform_oauth_grants_scopes CHECK (cardinality(scopes) BETWEEN 1 AND 32),
    CONSTRAINT ck_platform_oauth_grants_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_platform_oauth_grants_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_platform_oauth_grants_revision CHECK (revision > 0)
);
CREATE UNIQUE INDEX uq_platform_oauth_grants_active_account
    ON platform_oauth_grants (app_id, account_id) WHERE target = 'USER' AND status = 'ACTIVE';
CREATE UNIQUE INDEX uq_platform_oauth_grants_active_organization
    ON platform_oauth_grants (app_id, organization_id) WHERE target = 'ORGANIZATION' AND status = 'ACTIVE';
CREATE INDEX idx_platform_oauth_grants_account ON platform_oauth_grants (account_id, created_at);
CREATE INDEX idx_platform_oauth_grants_organization ON platform_oauth_grants (organization_id, created_at);
CREATE INDEX idx_platform_oauth_grants_app ON platform_oauth_grants (app_id, created_at, id);
CREATE TRIGGER trg_platform_oauth_grants_immutable
    BEFORE UPDATE ON platform_oauth_grants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'app_id', 'target', 'account_id', 'organization_id', 'installed_by_account_id',
        'tenant_id', 'application_id', 'environment_id', 'principal_id', 'service_account_id',
        'created_at');
CREATE TRIGGER trg_platform_oauth_grants_revoked_terminal
    BEFORE UPDATE ON platform_oauth_grants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_leaving_terminal_status('REVOKED');
CREATE TRIGGER trg_platform_oauth_grants_revision
    BEFORE UPDATE ON platform_oauth_grants
    FOR EACH ROW EXECUTE FUNCTION platform_bump_revision();
CREATE TRIGGER trg_platform_oauth_grants_change AFTER INSERT OR UPDATE OR DELETE ON platform_oauth_grants
    FOR EACH ROW EXECUTE FUNCTION platform_record_identity_change('APP_GRANT', 'id');

CREATE TABLE platform_oauth_codes (
    id                   UUID         NOT NULL,
    public_id            VARCHAR(32)  NOT NULL,
    environment_tag      VARCHAR(8)   NOT NULL,
    verifier             BYTEA        NOT NULL,
    verifier_key_version INTEGER      NOT NULL,
    grant_id             UUID         NOT NULL,
    app_id               UUID         NOT NULL,
    redirect_uri         TEXT         NOT NULL,
    code_challenge       VARCHAR(128) NOT NULL,
    scopes               TEXT[]       NOT NULL,
    expires_at           TIMESTAMPTZ  NOT NULL,
    consumed_at          TIMESTAMPTZ,
    created_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_oauth_codes PRIMARY KEY (id),
    CONSTRAINT fk_platform_oauth_codes_grant
        FOREIGN KEY (grant_id) REFERENCES platform_oauth_grants (id) ON DELETE CASCADE,
    CONSTRAINT uq_platform_oauth_codes_public_id UNIQUE (public_id),
    CONSTRAINT ck_platform_oauth_codes_public_id CHECK (public_id ~ '^[0-9a-f]{24}$'),
    CONSTRAINT ck_platform_oauth_codes_tag CHECK (environment_tag IN ('sbx', 'test', 'live')),
    CONSTRAINT ck_platform_oauth_codes_verifier CHECK (octet_length(verifier) = 32)
);
CREATE INDEX idx_platform_oauth_codes_expires ON platform_oauth_codes (expires_at);
CREATE INDEX idx_platform_oauth_codes_grant ON platform_oauth_codes (grant_id);

CREATE TABLE platform_oauth_tokens (
    credential_id UUID         NOT NULL,
    grant_id      UUID         NOT NULL,
    kind          TEXT         NOT NULL,
    created_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_oauth_tokens PRIMARY KEY (credential_id),
    CONSTRAINT fk_platform_oauth_tokens_credential
        FOREIGN KEY (credential_id) REFERENCES platform_api_credentials (id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_oauth_tokens_grant
        FOREIGN KEY (grant_id) REFERENCES platform_oauth_grants (id) ON DELETE RESTRICT,
    CONSTRAINT ck_platform_oauth_tokens_kind CHECK (kind IN ('ACCESS', 'BOT'))
);
CREATE INDEX idx_platform_oauth_tokens_grant ON platform_oauth_tokens (grant_id, kind);
CREATE TRIGGER trg_platform_oauth_tokens_immutable
    BEFORE UPDATE OR DELETE ON platform_oauth_tokens
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_mutation();

CREATE TABLE platform_oauth_refresh_tokens (
    id                   UUID         NOT NULL,
    public_id            VARCHAR(32)  NOT NULL,
    environment_tag      VARCHAR(8)   NOT NULL,
    verifier             BYTEA        NOT NULL,
    verifier_key_version INTEGER      NOT NULL,
    grant_id             UUID         NOT NULL,
    expires_at           TIMESTAMPTZ  NOT NULL,
    superseded_at        TIMESTAMPTZ,
    superseded_by_id     UUID,
    revoked_at           TIMESTAMPTZ,
    created_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_platform_oauth_refresh_tokens PRIMARY KEY (id),
    CONSTRAINT fk_platform_oauth_refresh_tokens_grant
        FOREIGN KEY (grant_id) REFERENCES platform_oauth_grants (id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_oauth_refresh_tokens_public_id UNIQUE (public_id),
    CONSTRAINT ck_platform_oauth_refresh_tokens_public_id CHECK (public_id ~ '^[0-9a-f]{24}$'),
    CONSTRAINT ck_platform_oauth_refresh_tokens_tag CHECK (environment_tag IN ('sbx', 'test', 'live')),
    CONSTRAINT ck_platform_oauth_refresh_tokens_verifier CHECK (octet_length(verifier) = 32),
    CONSTRAINT ck_platform_oauth_refresh_tokens_superseded
        CHECK ((superseded_at IS NULL) = (superseded_by_id IS NULL))
);
CREATE INDEX idx_platform_oauth_refresh_tokens_grant ON platform_oauth_refresh_tokens (grant_id);
CREATE TRIGGER trg_platform_oauth_refresh_tokens_immutable
    BEFORE UPDATE ON platform_oauth_refresh_tokens
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'public_id', 'environment_tag', 'verifier', 'verifier_key_version', 'grant_id',
        'expires_at', 'created_at');
CREATE TRIGGER trg_platform_oauth_refresh_tokens_set_once
    BEFORE UPDATE ON platform_oauth_refresh_tokens
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('superseded_at', 'superseded_by_id', 'revoked_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_oauth_refresh_tokens;
DROP TABLE IF EXISTS platform_oauth_tokens;
DROP TABLE IF EXISTS platform_oauth_codes;
DROP TABLE IF EXISTS platform_oauth_grants;
DROP TABLE IF EXISTS platform_oauth_client_secrets;
DROP TABLE IF EXISTS platform_oauth_clients;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_identity_changes DROP CONSTRAINT ck_platform_identity_changes_kind; \
             ALTER TABLE platform_identity_changes ADD CONSTRAINT ck_platform_identity_changes_kind \
             CHECK (entity_kind IN ({KINDS_V1}, 'APP_GRANT'));"
        ))
        .await?;
        db.execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM platform_oauth_clients) \
                  OR EXISTS (SELECT 1 FROM platform_identity_changes WHERE entity_kind = 'APP_GRANT') THEN \
                 RAISE EXCEPTION 'OAuth clients or app grants exist; refusing to downgrade'; \
               END IF; END $$;",
        )
        .await?;
        db.execute_unprepared(DOWN).await?;
        db.execute_unprepared(&format!(
            "ALTER TABLE platform_identity_changes DROP CONSTRAINT ck_platform_identity_changes_kind; \
             ALTER TABLE platform_identity_changes ADD CONSTRAINT ck_platform_identity_changes_kind \
             CHECK (entity_kind IN ({KINDS_V1}));"
        ))
        .await?;
        Ok(())
    }
}
