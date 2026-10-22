//! Part V Appendix J — envelope encryption of MANAGED content.
//!
//! `messaging_managed_deks` holds one data-encryption key (DEK) per application
//! environment, WRAPPED by the key-encryption key (KEK) of a pluggable key provider (the
//! deployment's key-ring file, or an external KMS / HSM implementation of the provider
//! interface). PostgreSQL never holds a usable key: without the provider's KEK a wrapped
//! DEK opens nothing. New MANAGED content is sealed under the environment's DEK (key id
//! `dek:<id>`); content sealed before this migration keeps opening with the key ring.
//!
//! Rows are immutable except for `status` (ACTIVE → RETIRED) and are never deleted;
//! exactly one ACTIVE DEK per environment. Key-id columns widen to 48 characters.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_managed_deks (
    id              UUID         NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    kek_id          VARCHAR(64)  NOT NULL,
    wrap_nonce      BYTEA        NOT NULL,
    wrapped_dek     BYTEA        NOT NULL,
    status          VARCHAR(16)  NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_managed_deks PRIMARY KEY (id),
    CONSTRAINT ck_messaging_managed_deks_status CHECK (status IN ('ACTIVE', 'RETIRED')),
    CONSTRAINT ck_messaging_managed_deks_wrapped CHECK (octet_length(wrapped_dek) = 48 AND octet_length(wrap_nonce) = 24)
);
CREATE UNIQUE INDEX uq_messaging_managed_deks_active ON messaging_managed_deks (environment_id) WHERE status = 'ACTIVE';

CREATE FUNCTION messaging_managed_deks_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'messaging: data-encryption keys are never deleted' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.environment_id <> OLD.environment_id OR NEW.kek_id <> OLD.kek_id
       OR NEW.wrap_nonce <> OLD.wrap_nonce OR NEW.wrapped_dek <> OLD.wrapped_dek THEN
        RAISE EXCEPTION 'messaging: a wrapped data-encryption key is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.status = 'RETIRED' AND NEW.status <> 'RETIRED' THEN
        RAISE EXCEPTION 'messaging: a retired data-encryption key stays retired' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_managed_deks_guard BEFORE UPDATE OR DELETE ON messaging_managed_deks
    FOR EACH ROW EXECUTE FUNCTION messaging_managed_deks_guard();

ALTER TABLE messaging_messages ALTER COLUMN managed_seal_key_id TYPE VARCHAR(48);
ALTER TABLE messaging_message_revisions ALTER COLUMN managed_seal_key_id TYPE VARCHAR(48);
ALTER TABLE messaging_reports ALTER COLUMN evidence_key_id TYPE VARCHAR(48);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_managed_deks;
DROP FUNCTION IF EXISTS messaging_managed_deks_guard();
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
