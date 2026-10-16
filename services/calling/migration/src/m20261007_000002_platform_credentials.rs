//! API credentials and their granted scopes.
//!
//! The raw credential secret is NEVER stored: a credential row carries only a public
//! lookup identifier, a keyed HMAC-SHA-256 verifier (with algorithm and key version)
//! and the last four characters for display. Ownership (tenant / application /
//! environment / service account) is pinned by a composite foreign key and is
//! immutable, as are the verifier and validity window. Revocation is terminal and
//! rotation links (`rotated_from` / `superseded_by`) can be written once only.
//!
//! A credential's granted scopes reference the application's allowed-scope rows, so a
//! credential can never hold a scope its application does not allow; withdrawing an
//! application scope withdraws it from every credential immediately (cascade).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_api_credentials (
    id                          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    public_id                   VARCHAR(32)  NOT NULL,
    tenant_id                   UUID         NOT NULL,
    application_id              UUID         NOT NULL,
    environment_id              UUID         NOT NULL,
    service_account_id          UUID         NOT NULL,
    label                       VARCHAR(100),
    verifier                    BYTEA        NOT NULL,
    verifier_algorithm          TEXT         NOT NULL,
    verifier_key_version        INTEGER      NOT NULL,
    last_four                   CHAR(4)      NOT NULL,
    status                      TEXT         NOT NULL DEFAULT 'ACTIVE',
    not_before                  TIMESTAMPTZ  NOT NULL,
    expires_at                  TIMESTAMPTZ  NOT NULL,
    revoked_at                  TIMESTAMPTZ,
    rotated_from_credential_id  UUID,
    superseded_by_credential_id UUID,
    superseded_at               TIMESTAMPTZ,
    supersession_valid_until    TIMESTAMPTZ,
    created_at                  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_api_credentials_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    -- Rotation can never cross a service account / environment / application / tenant.
    CONSTRAINT fk_platform_api_credentials_rotated_from
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id, rotated_from_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_api_credentials_superseded_by
        FOREIGN KEY (tenant_id, application_id, environment_id, service_account_id, superseded_by_credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, environment_id, service_account_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_api_credentials_public_id UNIQUE (public_id),
    CONSTRAINT uq_platform_api_credentials_owner_id
        UNIQUE (tenant_id, application_id, environment_id, service_account_id, id),
    CONSTRAINT uq_platform_api_credentials_application_id UNIQUE (tenant_id, application_id, id),
    -- A credential is rotated at most once (one successor) and has at most one predecessor.
    CONSTRAINT uq_platform_api_credentials_rotated_from UNIQUE (rotated_from_credential_id),
    CONSTRAINT uq_platform_api_credentials_superseded_by UNIQUE (superseded_by_credential_id),
    CONSTRAINT ck_platform_api_credentials_public_id CHECK (public_id ~ '^[0-9a-f]{24}$'),
    CONSTRAINT ck_platform_api_credentials_verifier_length CHECK (octet_length(verifier) = 32),
    CONSTRAINT ck_platform_api_credentials_verifier_algorithm CHECK (verifier_algorithm IN ('HMAC_SHA256')),
    CONSTRAINT ck_platform_api_credentials_verifier_key_version CHECK (verifier_key_version > 0),
    CONSTRAINT ck_platform_api_credentials_last_four CHECK (last_four ~ '^[0-9a-f]{4}$'),
    CONSTRAINT ck_platform_api_credentials_label CHECK (label IS NULL OR length(btrim(label)) BETWEEN 1 AND 100),
    CONSTRAINT ck_platform_api_credentials_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_platform_api_credentials_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_platform_api_credentials_validity_window CHECK (expires_at > not_before),
    CONSTRAINT ck_platform_api_credentials_no_self_link
        CHECK (rotated_from_credential_id IS DISTINCT FROM id AND superseded_by_credential_id IS DISTINCT FROM id),
    CONSTRAINT ck_platform_api_credentials_supersession_complete
        CHECK ((superseded_by_credential_id IS NULL) = (superseded_at IS NULL)
               AND (superseded_at IS NULL) = (supersession_valid_until IS NULL)),
    -- Rotation overlap never extends a credential beyond its original expiry.
    CONSTRAINT ck_platform_api_credentials_supersession_never_extends
        CHECK (supersession_valid_until IS NULL OR supersession_valid_until <= expires_at)
);
CREATE INDEX idx_platform_api_credentials_service_account ON platform_api_credentials (service_account_id);
CREATE INDEX idx_platform_api_credentials_application_environment
    ON platform_api_credentials (application_id, environment_id);
CREATE TRIGGER trg_platform_api_credentials_immutable
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'public_id', 'tenant_id', 'application_id', 'environment_id', 'service_account_id',
        'verifier', 'verifier_algorithm', 'verifier_key_version', 'last_four',
        'not_before', 'expires_at', 'rotated_from_credential_id', 'created_at');
CREATE TRIGGER trg_platform_api_credentials_set_once
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment(
        'superseded_by_credential_id', 'superseded_at', 'supersession_valid_until', 'revoked_at');
CREATE TRIGGER trg_platform_api_credentials_revoked_terminal
    BEFORE UPDATE ON platform_api_credentials
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_leaving_terminal_status('REVOKED');

CREATE TABLE platform_api_credential_scopes (
    credential_id  UUID         NOT NULL,
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    scope          VARCHAR(100) NOT NULL,
    granted_at     TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_platform_api_credential_scopes PRIMARY KEY (credential_id, scope),
    CONSTRAINT fk_platform_api_credential_scopes_credential
        FOREIGN KEY (tenant_id, application_id, credential_id)
        REFERENCES platform_api_credentials (tenant_id, application_id, id) ON DELETE CASCADE,
    -- Subset rule: a credential scope must be an allowed scope of the same application.
    CONSTRAINT fk_platform_api_credential_scopes_application_scope
        FOREIGN KEY (application_id, scope)
        REFERENCES platform_application_scopes (application_id, scope) ON DELETE CASCADE
);
CREATE INDEX idx_platform_api_credential_scopes_application_scope
    ON platform_api_credential_scopes (application_id, scope);
CREATE TRIGGER trg_platform_api_credential_scopes_immutable
    BEFORE UPDATE ON platform_api_credential_scopes
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'credential_id', 'tenant_id', 'application_id', 'scope', 'granted_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_api_credential_scopes;
DROP TABLE IF EXISTS platform_api_credentials;
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
