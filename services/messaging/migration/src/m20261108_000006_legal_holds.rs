//! Legal and compliance holds (Part V §91).
//!
//! A hold preserves data from purge: messages (ciphertext of PRIVATE_E2EE, sealed text of
//! MANAGED), attachment objects and their metadata, within a scope — one conversation,
//! one principal's messages and uploads, or a whole application environment. Holds are
//! created and released by authorized administration only, recorded with actor, reason
//! category, opaque case reference, creation and optional expiry, and are never deleted:
//! the only change a row admits is ACTIVE → RELEASED (with who, when and why) — a trigger
//! enforces it. A hold NEVER creates any decryption capability.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_legal_holds (
    id                         UUID          NOT NULL,
    tenant_id                  UUID          NOT NULL,
    application_id             UUID          NOT NULL,
    environment_id             UUID          NOT NULL,
    scope_kind                 VARCHAR(16)   NOT NULL,
    conversation_id            UUID,
    principal_id               UUID,
    reason_category            VARCHAR(24)   NOT NULL,
    case_reference             VARCHAR(128),
    status                     VARCHAR(16)   NOT NULL DEFAULT 'ACTIVE',
    created_by_credential_id   UUID          NOT NULL,
    created_by_principal_id    UUID,
    created_at                 TIMESTAMPTZ   NOT NULL,
    expires_at                 TIMESTAMPTZ,
    released_at                TIMESTAMPTZ,
    released_by_credential_id  UUID,
    release_reason             VARCHAR(48),
    CONSTRAINT pk_messaging_legal_holds PRIMARY KEY (id),
    CONSTRAINT ck_messaging_legal_holds_scope CHECK (
        (scope_kind = 'CONVERSATION' AND conversation_id IS NOT NULL AND principal_id IS NULL)
        OR (scope_kind = 'PRINCIPAL' AND principal_id IS NOT NULL AND conversation_id IS NULL)
        OR (scope_kind = 'ENVIRONMENT' AND conversation_id IS NULL AND principal_id IS NULL)),
    CONSTRAINT ck_messaging_legal_holds_reason CHECK (reason_category IN ('LITIGATION', 'REGULATORY', 'INVESTIGATION', 'INTERNAL_POLICY')),
    CONSTRAINT ck_messaging_legal_holds_case CHECK (case_reference IS NULL OR case_reference ~ '^[A-Za-z0-9._:/-]{1,128}$'),
    CONSTRAINT ck_messaging_legal_holds_status CHECK (status IN ('ACTIVE', 'RELEASED')),
    CONSTRAINT ck_messaging_legal_holds_released CHECK ((status = 'RELEASED') = (released_at IS NOT NULL)
        AND (status = 'RELEASED') = (released_by_credential_id IS NOT NULL)
        AND (status = 'RELEASED') = (release_reason IS NOT NULL)),
    CONSTRAINT ck_messaging_legal_holds_release_reason CHECK (release_reason IS NULL OR release_reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_legal_holds_expiry CHECK (expires_at IS NULL OR expires_at > created_at)
);
CREATE INDEX ix_messaging_legal_holds_active ON messaging_legal_holds (environment_id) WHERE status = 'ACTIVE';
CREATE FUNCTION messaging_legal_holds_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: legal holds are never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.status = 'RELEASED' THEN
        RAISE EXCEPTION 'messaging: a released legal hold is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.scope_kind <> OLD.scope_kind
       OR NEW.conversation_id IS DISTINCT FROM OLD.conversation_id OR NEW.principal_id IS DISTINCT FROM OLD.principal_id
       OR NEW.reason_category <> OLD.reason_category OR NEW.case_reference IS DISTINCT FROM OLD.case_reference
       OR NEW.created_by_credential_id <> OLD.created_by_credential_id OR NEW.created_at <> OLD.created_at
       OR NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'messaging: a legal hold can only be released' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_legal_holds_guard BEFORE UPDATE OR DELETE ON messaging_legal_holds
    FOR EACH ROW EXECUTE FUNCTION messaging_legal_holds_guard();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_legal_holds;
DROP FUNCTION IF EXISTS messaging_legal_holds_guard();
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
