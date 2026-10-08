//! Contact equality fingerprints (Part D §32).
//!
//! `contact_fingerprints` holds, per contact and fingerprint-key VERSION, the keyed,
//! scope-bound HMAC-SHA-256 of the canonical number (computed by Privacy Relay with a key
//! that never enters the database). Uniqueness is per (tenant, application,
//! environment, key version, fingerprint): exact-number deduplication inside one
//! application environment, and no equality identity across scopes. Several key
//! versions may coexist during a fingerprint-key rotation. There is no plaintext, plain
//! hash or number index.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE contact_fingerprints (
    contact_reference        UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    fingerprint_key_version  VARCHAR(63)  NOT NULL,
    fingerprint              BYTEA        NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    PRIMARY KEY (contact_reference, fingerprint_key_version),
    CONSTRAINT fk_contact_fingerprints_contact FOREIGN KEY (tenant_id, application_id, environment_id, contact_reference)
        REFERENCES protected_contacts (tenant_id, application_id, environment_id, id),
    CONSTRAINT uq_contact_fingerprints_scope UNIQUE (tenant_id, application_id, environment_id, fingerprint_key_version, fingerprint),
    CONSTRAINT ck_contact_fingerprints_version CHECK (fingerprint_key_version ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_contact_fingerprints_length CHECK (octet_length(fingerprint) = 32)
);
CREATE INDEX idx_contact_fingerprints_version ON contact_fingerprints (fingerprint_key_version);
CREATE TRIGGER trg_contact_fingerprints_immutable
    BEFORE UPDATE ON contact_fingerprints
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS contact_fingerprints;
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
