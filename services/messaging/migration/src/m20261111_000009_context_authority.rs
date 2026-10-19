//! Part V H §103 — communication-context authority of a peer service.
//!
//! `messaging_context_authorities` holds, per environment, the CURRENT authority state of
//! an opaque context reference owned by another service (namespace `privacy-relay`:
//! Privacy Relay's protected-communication contexts). It carries identifiers and states
//! only — never a telephone number, contact value or Contact Vault data.
//!
//! * Conversations in an authority-controlled namespace can only be created, and only
//!   reopened, while their context is ACTIVE.
//! * SUSPENDED locks them; REVOKED closes them (and keeps them closed).
//! * Updates are version-fenced: the stored `source_version` only moves forward
//!   (trigger), so a delayed or replayed update can never restore withdrawn authority.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_context_authorities (
    environment_id   UUID         NOT NULL,
    namespace        VARCHAR(63)  NOT NULL,
    reference        VARCHAR(256) NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    state            VARCHAR(16)  NOT NULL,
    source           VARCHAR(64)  NOT NULL,
    source_version   BIGINT       NOT NULL,
    reason           VARCHAR(48),
    updated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_context_authorities PRIMARY KEY (environment_id, namespace, reference),
    CONSTRAINT ck_messaging_context_authorities_state CHECK (state IN ('ACTIVE', 'SUSPENDED', 'REVOKED')),
    CONSTRAINT ck_messaging_context_authorities_version CHECK (source_version >= 0),
    CONSTRAINT ck_messaging_context_authorities_reason CHECK (reason IS NULL OR reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE FUNCTION messaging_context_authorities_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: context authority records are never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.source_version <= OLD.source_version OR NEW.source <> OLD.source THEN
        RAISE EXCEPTION 'messaging: context authority only moves forward from its own source' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_context_authorities_guard BEFORE UPDATE OR DELETE ON messaging_context_authorities
    FOR EACH ROW EXECUTE FUNCTION messaging_context_authorities_guard();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_context_authorities;
DROP FUNCTION IF EXISTS messaging_context_authorities_guard();
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
