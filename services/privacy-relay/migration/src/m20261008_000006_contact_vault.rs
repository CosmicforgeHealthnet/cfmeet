//! Contact Vault storage (Part D §31).
//!
//! * `protected_contacts`: one protected telephone contact per ContactReference (the row
//!   id: an opaque, server-generated UUID scoped to tenant / application / environment).
//!   The canonical E.164 value exists ONLY as AES-256-GCM ciphertext (+ its 96-bit nonce
//!   and the key VERSION — never key bytes). There is deliberately no plaintext, number,
//!   e164 or raw column. Safe metadata: a bounded masked display, the country, the voice
//!   capability (UNKNOWN unless a trusted source says otherwise) and the verification
//!   state (§33).
//! * `contact_access_audit`: append-only evidence of every sensitive Contact Vault
//!   operation (who, which ContactReference, which operation / purpose / execution
//!   references, outcome, request / correlation). It never holds a contact value.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE protected_contacts (
    id                       UUID PRIMARY KEY,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    version                  BIGINT       NOT NULL,
    ciphertext               BYTEA        NOT NULL,
    nonce                    BYTEA        NOT NULL,
    encryption_algorithm     TEXT         NOT NULL,
    encryption_key_version   VARCHAR(63)  NOT NULL,
    masked_display           VARCHAR(32)  NOT NULL,
    country_iso2             CHAR(2),
    voice_capability         TEXT         NOT NULL,
    verification_state       TEXT         NOT NULL,
    verification_expires_at  TIMESTAMPTZ,
    verification_updated_at  TIMESTAMPTZ  NOT NULL,
    disabled_at              TIMESTAMPTZ,
    created_at               TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    CONSTRAINT uq_protected_contacts_scope_id UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_protected_contacts_version CHECK (version >= 1),
    CONSTRAINT ck_protected_contacts_ciphertext CHECK (octet_length(ciphertext) BETWEEN 19 AND 64),
    CONSTRAINT ck_protected_contacts_nonce CHECK (octet_length(nonce) = 12),
    CONSTRAINT ck_protected_contacts_algorithm CHECK (encryption_algorithm = 'AES-256-GCM'),
    CONSTRAINT ck_protected_contacts_key_version CHECK (encryption_key_version ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    CONSTRAINT ck_protected_contacts_masked CHECK (masked_display ~ '^\+[0-9]{1,3}•{6}[0-9]{0,4}$'),
    CONSTRAINT ck_protected_contacts_country CHECK (country_iso2 IS NULL OR country_iso2 ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_protected_contacts_voice CHECK (voice_capability IN ('UNKNOWN', 'SUPPORTED', 'UNSUPPORTED')),
    CONSTRAINT ck_protected_contacts_verification CHECK (verification_state IN (
        'UNVERIFIED', 'PENDING', 'VERIFIED', 'EXPIRED', 'INVALID', 'BLOCKED')),
    CONSTRAINT ck_protected_contacts_verification_expiry CHECK (
        verification_expires_at IS NULL OR verification_state IN ('VERIFIED', 'EXPIRED')),
    CONSTRAINT ck_protected_contacts_times CHECK (updated_at >= created_at)
);
CREATE INDEX idx_protected_contacts_scope ON protected_contacts (tenant_id, application_id, environment_id, created_at, id);
CREATE INDEX idx_protected_contacts_verified_expiry ON protected_contacts (verification_expires_at)
    WHERE verification_state = 'VERIFIED' AND verification_expires_at IS NOT NULL;
CREATE INDEX idx_protected_contacts_key_version ON protected_contacts (encryption_key_version, id);

CREATE FUNCTION privacy_relay_guard_contact_version() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.version NOT IN (OLD.version, OLD.version + 1) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: contact versions increase by one'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_protected_contacts_version
    BEFORE UPDATE ON protected_contacts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_contact_version();
CREATE TRIGGER trg_protected_contacts_immutable
    BEFORE UPDATE ON protected_contacts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_at');
CREATE TRIGGER trg_protected_contacts_disabled_once
    BEFORE UPDATE ON protected_contacts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('disabled_at');
CREATE TRIGGER trg_protected_contacts_no_delete
    BEFORE DELETE ON protected_contacts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

CREATE TABLE contact_access_audit (
    id                   UUID PRIMARY KEY,
    tenant_id            UUID        NOT NULL,
    application_id       UUID        NOT NULL,
    environment_id       UUID        NOT NULL,
    contact_reference    UUID,
    actor_kind           TEXT        NOT NULL,
    actor_service        VARCHAR(64) NOT NULL,
    service_account_id   UUID,
    credential_ref       VARCHAR(64),
    operation            TEXT        NOT NULL,
    purpose              VARCHAR(64) NOT NULL,
    context_ref          UUID,
    relay_session_ref    UUID,
    decision_ref         UUID,
    call_ref             UUID,
    call_leg_ref         UUID,
    grant_ref            UUID,
    outcome              TEXT        NOT NULL,
    reason_code          VARCHAR(64),
    request_id           VARCHAR(36) NOT NULL,
    correlation_id       VARCHAR(37) NOT NULL,
    created_at           TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_contact_audit_actor CHECK (actor_kind IN ('DELEGATED_CLIENT', 'SERVICE', 'SYSTEM')),
    CONSTRAINT ck_contact_audit_service CHECK (actor_service ~ '^[a-z][a-z0-9.-]{0,63}$'),
    CONSTRAINT ck_contact_audit_credential CHECK (credential_ref IS NULL OR credential_ref ~ '^[A-Za-z0-9_-]{1,64}$'),
    CONSTRAINT ck_contact_audit_operation CHECK (operation IN (
        'CREATE', 'READ', 'UPDATE', 'DISABLE', 'VERIFY', 'REENCRYPT', 'FINGERPRINT_BACKFILL',
        'GRANT_FOR_EXECUTION', 'RESOLVE_FOR_EXECUTION', 'PROTECT_INBOUND', 'EXPIRE_VERIFICATION')),
    CONSTRAINT ck_contact_audit_purpose CHECK (purpose ~ '^[A-Z][A-Z_]{0,63}$'),
    CONSTRAINT ck_contact_audit_outcome CHECK (outcome IN ('SUCCEEDED', 'DENIED', 'FAILED')),
    CONSTRAINT ck_contact_audit_reason CHECK (reason_code IS NULL OR reason_code ~ '^[A-Z][A-Z_]{0,63}$'),
    CONSTRAINT ck_contact_audit_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
CREATE INDEX idx_contact_audit_contact ON contact_access_audit (contact_reference, created_at)
    WHERE contact_reference IS NOT NULL;
CREATE INDEX idx_contact_audit_scope ON contact_access_audit (tenant_id, application_id, environment_id, created_at);
CREATE TRIGGER trg_contact_audit_append_only
    BEFORE UPDATE OR DELETE ON contact_access_audit
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS contact_access_audit;
DROP TABLE IF EXISTS protected_contacts;
DROP FUNCTION IF EXISTS privacy_relay_guard_contact_version();
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
