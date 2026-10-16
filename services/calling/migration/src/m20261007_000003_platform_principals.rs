//! Communication principals: the participants/entities a communication refers to.
//!
//! A principal is always scoped to one tenant / application / environment and is
//! identified there by `(kind, external_reference)`, where `external_reference` is an
//! opaque, consumer-defined (or platform-issued) handle. CF Meet attaches no business
//! meaning to it.
//!
//! Plaintext contact data is structurally excluded:
//! * there is no phone / E.164 / contact column;
//! * no reference may carry an E.164-style `+<digit>` sequence or a `tel:` / `sip:` URI;
//! * a TELEPHONE_ENDPOINT reference must be an opaque UUID handle (issued by the future
//!   Contact Vault), so it can never be a telephone number.
//!
//! SERVICE_ACCOUNT principals are linked explicitly to the authoritative service account
//! (same tenant / application / environment, composite FK); their reference is that
//! service account's id, never free text.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE platform_principals (
    id                        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id                 UUID         NOT NULL,
    application_id            UUID         NOT NULL,
    environment_id            UUID         NOT NULL,
    kind                      TEXT         NOT NULL,
    external_reference        VARCHAR(255) NOT NULL,
    linked_service_account_id UUID,
    status                    TEXT         NOT NULL DEFAULT 'ACTIVE',
    created_at                TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT fk_platform_principals_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_platform_principals_service_account
        FOREIGN KEY (tenant_id, application_id, environment_id, linked_service_account_id)
        REFERENCES platform_service_accounts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_platform_principals_scoped_reference
        UNIQUE (tenant_id, application_id, environment_id, kind, external_reference),
    CONSTRAINT uq_platform_principals_linked_service_account UNIQUE (linked_service_account_id),
    CONSTRAINT ck_platform_principals_kind CHECK (kind IN (
        'NATIVE_CF_USER',
        'EXTERNAL_APPLICATION_PRINCIPAL',
        'ENTERPRISE_MEMBER',
        'SERVICE_ACCOUNT',
        'TELEPHONE_ENDPOINT',
        'TEMPORARY_EXTERNAL_PARTICIPANT',
        'BOT_OR_SERVICE')),
    CONSTRAINT ck_platform_principals_status CHECK (status IN ('ACTIVE', 'SUSPENDED', 'DEACTIVATED')),
    CONSTRAINT ck_platform_principals_reference_format CHECK (external_reference ~ '^[!-~]{1,255}$'),
    CONSTRAINT ck_platform_principals_reference_not_telephone
        CHECK (external_reference !~ '\+[0-9]' AND external_reference !~* '^(tel|sip|sips):'),
    CONSTRAINT ck_platform_principals_telephone_endpoint_opaque
        CHECK (kind <> 'TELEPHONE_ENDPOINT'
               OR external_reference ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    CONSTRAINT ck_platform_principals_service_account_link
        CHECK ((kind = 'SERVICE_ACCOUNT') = (linked_service_account_id IS NOT NULL)),
    CONSTRAINT ck_platform_principals_service_account_reference
        CHECK (kind <> 'SERVICE_ACCOUNT' OR external_reference = linked_service_account_id::text)
);
CREATE INDEX idx_platform_principals_environment ON platform_principals (environment_id);
CREATE TRIGGER trg_platform_principals_immutable
    BEFORE UPDATE ON platform_principals
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'kind',
        'external_reference', 'linked_service_account_id', 'created_at');
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS platform_principals;
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
