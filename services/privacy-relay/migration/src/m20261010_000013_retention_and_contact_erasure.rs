//! Part H §80 / §31: data retention, contact-value erasure and audit hardening.
//!
//! * `protected_contacts` can be ERASED (minimized): a contact DISABLED for longer than
//!   the configured delay loses its ciphertext, nonce and key version (all NULL iff
//!   `erased_at` is set) and keeps only the opaque ContactReference, its country and a
//!   masked display without digits (`+<country code>••••••`). Erasure happens once
//!   (`erased_at` is set-once) and only for disabled contacts.
//! * `privacy_relay_retention_holds`: a hold (policy review, dispute, security
//!   investigation, other) that the retention workers respect — for one resource, one
//!   scope or one data class (`*` = every class).
//! * `privacy_relay_retention_audit`: append-only evidence of every retention batch
//!   (class, action, count, policy) — never a value.
//! * Retention deletes: the append-only / no-delete triggers
//!   (`privacy_relay_forbid_mutation`) allow a DELETE only inside a transaction of the
//!   retention worker (`SET LOCAL privacy_relay.retention_worker = 'on'`); application
//!   code never sets it. UPDATE stays refused unconditionally; the retention audit and
//!   the holds table are append-only for everyone.
//! * TRUNCATE is refused on every evidence table (statement-level triggers).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Retention worker flag: DELETE is allowed only inside a retention transaction.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION privacy_relay_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND current_setting('privacy_relay.retention_worker', true) = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'privacy relay invariant violated: % rows are append-only (% refused)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

-- Unconditional append-only (retention evidence and holds are never deleted).
CREATE FUNCTION privacy_relay_forbid_mutation_always() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'privacy relay invariant violated: % rows are append-only (% refused)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;

-- TRUNCATE is never a retention operation.
CREATE FUNCTION privacy_relay_forbid_truncate() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'privacy relay invariant violated: % cannot be truncated', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;
CREATE TRIGGER trg_contact_audit_no_truncate
    BEFORE TRUNCATE ON contact_access_audit
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();
CREATE TRIGGER trg_relay_audit_no_truncate
    BEFORE TRUNCATE ON relay_audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();
CREATE TRIGGER trg_contact_verification_no_truncate
    BEFORE TRUNCATE ON contact_verification_events
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();
CREATE TRIGGER trg_relay_decisions_no_truncate
    BEFORE TRUNCATE ON relay_authorization_decisions
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();
CREATE TRIGGER trg_protected_contacts_no_truncate
    BEFORE TRUNCATE ON protected_contacts
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();

-- ---------------------------------------------------------------------------
-- Retention holds.
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_retention_holds (
    id              UUID PRIMARY KEY,
    data_class      VARCHAR(64)  NOT NULL,
    resource_id     UUID,
    tenant_id       UUID,
    application_id  UUID,
    environment_id  UUID,
    reason_code     TEXT         NOT NULL,
    created_by      VARCHAR(64)  NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    released_at     TIMESTAMPTZ,
    CONSTRAINT ck_retention_holds_class CHECK (data_class ~ '^(\*|[A-Z][A-Z_]{0,63})$'),
    CONSTRAINT ck_retention_holds_reason CHECK (reason_code IN (
        'POLICY_REVIEW', 'DISPUTE', 'SECURITY_INVESTIGATION', 'OTHER')),
    CONSTRAINT ck_retention_holds_created_by CHECK (created_by ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$'),
    CONSTRAINT ck_retention_holds_scope CHECK (
        (application_id IS NULL OR tenant_id IS NOT NULL)
        AND (environment_id IS NULL OR application_id IS NOT NULL)),
    CONSTRAINT ck_retention_holds_released CHECK (released_at IS NULL OR released_at >= created_at)
);
CREATE INDEX idx_retention_holds_active ON privacy_relay_retention_holds (data_class) WHERE released_at IS NULL;
CREATE TRIGGER trg_retention_holds_immutable
    BEFORE UPDATE ON privacy_relay_retention_holds
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'data_class', 'resource_id', 'tenant_id', 'application_id', 'environment_id',
        'reason_code', 'created_by', 'created_at');
CREATE TRIGGER trg_retention_holds_released_once
    BEFORE UPDATE ON privacy_relay_retention_holds
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('released_at');
CREATE TRIGGER trg_retention_holds_no_delete
    BEFORE DELETE ON privacy_relay_retention_holds
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation_always();
CREATE TRIGGER trg_retention_holds_no_truncate
    BEFORE TRUNCATE ON privacy_relay_retention_holds
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();

-- Whether an active hold covers (class, resource, scope). A hold matches when every
-- member it names equals the row's member; a NULL member means "any".
CREATE FUNCTION privacy_relay_retention_held(class text, resource uuid, tenant uuid, app uuid, env uuid)
RETURNS boolean LANGUAGE sql STABLE AS $$
    SELECT EXISTS (
        SELECT 1 FROM privacy_relay_retention_holds h
        WHERE h.released_at IS NULL
          AND (h.data_class = '*' OR h.data_class = class)
          AND (h.resource_id IS NULL OR h.resource_id = resource)
          AND (h.tenant_id IS NULL OR h.tenant_id = tenant)
          AND (h.application_id IS NULL OR h.application_id = app)
          AND (h.environment_id IS NULL OR h.environment_id = env))
$$;

-- ---------------------------------------------------------------------------
-- Retention audit (append-only, never a value).
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_retention_audit (
    id            UUID PRIMARY KEY,
    data_class    VARCHAR(64)  NOT NULL,
    action        TEXT         NOT NULL,
    resource_ref  UUID,
    batch_count   INTEGER      NOT NULL,
    policy        VARCHAR(128) NOT NULL,
    actor         TEXT         NOT NULL,
    outcome       TEXT         NOT NULL,
    occurred_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT ck_retention_audit_class CHECK (data_class ~ '^(\*|[A-Z][A-Z_]{0,63})$'),
    CONSTRAINT ck_retention_audit_action CHECK (action IN ('DELETE', 'MINIMIZE', 'HOLD', 'RELEASE')),
    CONSTRAINT ck_retention_audit_count CHECK (batch_count >= 0),
    CONSTRAINT ck_retention_audit_policy CHECK (policy ~ '^[A-Z][A-Z0-9_]{0,63}(=[0-9]{1,10})?$'),
    CONSTRAINT ck_retention_audit_actor CHECK (actor IN ('SYSTEM', 'OPERATOR')),
    CONSTRAINT ck_retention_audit_outcome CHECK (outcome IN ('SUCCEEDED', 'FAILED'))
);
CREATE INDEX idx_retention_audit_time ON privacy_relay_retention_audit (occurred_at);
CREATE TRIGGER trg_retention_audit_append_only
    BEFORE UPDATE OR DELETE ON privacy_relay_retention_audit
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation_always();
CREATE TRIGGER trg_retention_audit_no_truncate
    BEFORE TRUNCATE ON privacy_relay_retention_audit
    FOR EACH STATEMENT EXECUTE FUNCTION privacy_relay_forbid_truncate();

-- ---------------------------------------------------------------------------
-- Contact erasure (minimization of a disabled contact's value).
-- ---------------------------------------------------------------------------
ALTER TABLE protected_contacts
    ADD COLUMN erased_at TIMESTAMPTZ,
    ALTER COLUMN ciphertext DROP NOT NULL,
    ALTER COLUMN nonce DROP NOT NULL,
    ALTER COLUMN encryption_key_version DROP NOT NULL,
    DROP CONSTRAINT ck_protected_contacts_ciphertext,
    DROP CONSTRAINT ck_protected_contacts_nonce,
    DROP CONSTRAINT ck_protected_contacts_key_version,
    DROP CONSTRAINT ck_protected_contacts_masked,
    ADD CONSTRAINT ck_protected_contacts_ciphertext CHECK (
        ciphertext IS NULL OR octet_length(ciphertext) BETWEEN 19 AND 64),
    ADD CONSTRAINT ck_protected_contacts_nonce CHECK (nonce IS NULL OR octet_length(nonce) = 12),
    ADD CONSTRAINT ck_protected_contacts_key_version CHECK (
        encryption_key_version IS NULL OR encryption_key_version ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    -- A live contact keeps the bounded masked display; an erased one keeps only the
    -- country calling code (or nothing).
    ADD CONSTRAINT ck_protected_contacts_masked CHECK (
        (erased_at IS NULL AND masked_display ~ '^\+[0-9]{1,3}•{6}[0-9]{0,4}$')
        OR (erased_at IS NOT NULL AND masked_display ~ '^(\+[0-9]{1,3}•{6}|•••)$')),
    -- Ciphertext, nonce and key version are all present or all gone, exactly when erased;
    -- only a disabled contact is ever erased.
    ADD CONSTRAINT ck_protected_contacts_erased CHECK (
        (erased_at IS NULL) = (ciphertext IS NOT NULL)
        AND (erased_at IS NULL) = (nonce IS NOT NULL)
        AND (erased_at IS NULL) = (encryption_key_version IS NOT NULL)
        AND (erased_at IS NULL OR (disabled_at IS NOT NULL AND erased_at >= disabled_at)));
CREATE INDEX idx_protected_contacts_erasure ON protected_contacts (disabled_at)
    WHERE disabled_at IS NOT NULL AND erased_at IS NULL;
CREATE TRIGGER trg_protected_contacts_erased_once
    BEFORE UPDATE ON protected_contacts
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('erased_at');

ALTER TABLE contact_access_audit DROP CONSTRAINT ck_contact_audit_operation;
ALTER TABLE contact_access_audit ADD CONSTRAINT ck_contact_audit_operation CHECK (operation IN (
    'CREATE', 'READ', 'UPDATE', 'DISABLE', 'VERIFY', 'REENCRYPT', 'FINGERPRINT_BACKFILL',
    'GRANT_FOR_EXECUTION', 'RESOLVE_FOR_EXECUTION', 'PROTECT_INBOUND', 'EXPIRE_VERIFICATION', 'ERASE'));

-- Retention deletes of grants: (status, time) lookups.
CREATE INDEX idx_contact_grants_retention ON contact_resolution_grants (expires_at) WHERE status <> 'ISSUED';
CREATE INDEX idx_contact_audit_time ON contact_access_audit (created_at);
CREATE INDEX idx_relay_outbox_published ON privacy_relay_outbox (published_at) WHERE status = 'PUBLISHED';
CREATE INDEX idx_communication_contexts_terminal ON communication_contexts (updated_at)
    WHERE status IN ('EXPIRED', 'REVOKED', 'COMPLETED');
"#;

const DOWN: &str = r#"
-- Erasure is irreversible: a schema without `erased_at` cannot hold an erased contact.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM protected_contacts WHERE erased_at IS NOT NULL) THEN
        RAISE EXCEPTION 'privacy relay: erased contacts exist; this migration cannot be reverted'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
END;
$$;
DROP INDEX IF EXISTS idx_communication_contexts_terminal;
DROP INDEX IF EXISTS idx_relay_outbox_published;
DROP INDEX IF EXISTS idx_contact_audit_time;
DROP INDEX IF EXISTS idx_contact_grants_retention;
-- Audit rows are immutable evidence: the narrower constraint applies to new rows only.
ALTER TABLE contact_access_audit DROP CONSTRAINT ck_contact_audit_operation;
ALTER TABLE contact_access_audit ADD CONSTRAINT ck_contact_audit_operation CHECK (operation IN (
    'CREATE', 'READ', 'UPDATE', 'DISABLE', 'VERIFY', 'REENCRYPT', 'FINGERPRINT_BACKFILL',
    'GRANT_FOR_EXECUTION', 'RESOLVE_FOR_EXECUTION', 'PROTECT_INBOUND', 'EXPIRE_VERIFICATION')) NOT VALID;
DROP TRIGGER IF EXISTS trg_protected_contacts_erased_once ON protected_contacts;
DROP INDEX IF EXISTS idx_protected_contacts_erasure;
ALTER TABLE protected_contacts
    DROP CONSTRAINT ck_protected_contacts_erased,
    DROP CONSTRAINT ck_protected_contacts_masked,
    DROP CONSTRAINT ck_protected_contacts_key_version,
    DROP CONSTRAINT ck_protected_contacts_nonce,
    DROP CONSTRAINT ck_protected_contacts_ciphertext,
    ADD CONSTRAINT ck_protected_contacts_ciphertext CHECK (octet_length(ciphertext) BETWEEN 19 AND 64),
    ADD CONSTRAINT ck_protected_contacts_nonce CHECK (octet_length(nonce) = 12),
    ADD CONSTRAINT ck_protected_contacts_key_version CHECK (encryption_key_version ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    ADD CONSTRAINT ck_protected_contacts_masked CHECK (masked_display ~ '^\+[0-9]{1,3}•{6}[0-9]{0,4}$'),
    ALTER COLUMN encryption_key_version SET NOT NULL,
    ALTER COLUMN nonce SET NOT NULL,
    ALTER COLUMN ciphertext SET NOT NULL,
    DROP COLUMN erased_at;
DROP TABLE IF EXISTS privacy_relay_retention_audit;
DROP FUNCTION IF EXISTS privacy_relay_retention_held(text, uuid, uuid, uuid, uuid);
DROP TABLE IF EXISTS privacy_relay_retention_holds;
DROP TRIGGER IF EXISTS trg_protected_contacts_no_truncate ON protected_contacts;
DROP TRIGGER IF EXISTS trg_relay_decisions_no_truncate ON relay_authorization_decisions;
DROP TRIGGER IF EXISTS trg_contact_verification_no_truncate ON contact_verification_events;
DROP TRIGGER IF EXISTS trg_relay_audit_no_truncate ON relay_audit_events;
DROP TRIGGER IF EXISTS trg_contact_audit_no_truncate ON contact_access_audit;
DROP FUNCTION IF EXISTS privacy_relay_forbid_truncate();
DROP FUNCTION IF EXISTS privacy_relay_forbid_mutation_always();
CREATE OR REPLACE FUNCTION privacy_relay_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'privacy relay invariant violated: % rows are append-only (% refused)', TG_TABLE_NAME, TG_OP
        USING ERRCODE = 'integrity_constraint_violation';
END;
$$;
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
