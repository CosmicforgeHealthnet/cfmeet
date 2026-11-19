//! Admin bridge foundation (ADR 0067, design §13, phase A1).
//!
//! * `admin_remote_principals` — enrolled HQ credentials: client-certificate fingerprints,
//!   HMAC key DIGESTS (the key lives only in a file), staff-token public keys. Never
//!   deleted; an UPDATE may only set `revoked_at` once.
//! * `admin_security_claims` — single-use nonces, jtis and approval ids (cleaned up).
//! * `admin_revocations` — staff / session revocations (append-only).
//! * `admin_rate_limits` — fixed one-minute windows (cleaned up).
//! * `admin_policy_revisions` — CF Meet-signed access policies (append-only; the stored
//!   digest must be the sha256 of the stored bytes).
//! * `admin_audit_entries` — hash-chained audit: no UPDATE, DELETE or TRUNCATE; every row's
//!   `previous_hash` must be the previous row's `entry_hash` (genesis: 64 zeros), checked
//!   under the same advisory lock the bridge takes. `scope` / `detail` are `json` (exact
//!   text) because the chain hashes their canonical serialization; `jsonb` would reorder
//!   keys.
//! * `admin_dataset_days`, `admin_projections` — small summaries for HQ (counts only).
//!
//! The bridge stores no business data, no personal content and no secret.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Advisory lock key serializing audit appends (bridge and trigger use the same key).
pub const AUDIT_LOCK_KEY: i64 = 0x0adb_1d6e_a0d1_7001;

const UP: &str = r#"
CREATE FUNCTION admin_append_only() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '%: append-only', TG_TABLE_NAME USING ERRCODE = 'check_violation';
END $$;

-- ---- Enrolled HQ credentials ------------------------------------------------------
CREATE TABLE admin_remote_principals (
    principal    VARCHAR(64)  NOT NULL,
    kind         VARCHAR(16)  NOT NULL,
    key_id       VARCHAR(128) NOT NULL,
    material     VARCHAR(64)  NOT NULL,
    issuer       VARCHAR(128),
    audience     VARCHAR(128),
    not_before   TIMESTAMPTZ  NOT NULL,
    expires_at   TIMESTAMPTZ  NOT NULL,
    revoked_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    created_by   VARCHAR(128) NOT NULL,
    CONSTRAINT pk_admin_remote_principals PRIMARY KEY (kind, key_id),
    CONSTRAINT ck_admin_remote_principals_principal CHECK (principal ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT ck_admin_remote_principals_kind CHECK (kind IN ('CLIENT_CERT', 'HMAC_KEY', 'STAFF_KEY')),
    CONSTRAINT ck_admin_remote_principals_key_id CHECK (key_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_admin_remote_principals_material CHECK (material ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_admin_remote_principals_cert_key_id CHECK (kind <> 'CLIENT_CERT' OR key_id = material),
    CONSTRAINT ck_admin_remote_principals_staff_claims CHECK (
        (kind = 'STAFF_KEY' AND issuer IS NOT NULL AND audience IS NOT NULL)
        OR (kind <> 'STAFF_KEY' AND issuer IS NULL AND audience IS NULL)),
    CONSTRAINT ck_admin_remote_principals_window CHECK (expires_at > not_before),
    CONSTRAINT ck_admin_remote_principals_created_by CHECK (char_length(btrim(created_by)) BETWEEN 1 AND 128)
);

CREATE FUNCTION admin_remote_principals_revoke_once() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.revoked_at IS NOT NULL OR NEW.revoked_at IS NULL
       OR (NEW.principal, NEW.kind, NEW.key_id, NEW.material, NEW.issuer, NEW.audience,
           NEW.not_before, NEW.expires_at, NEW.created_at, NEW.created_by)
          IS DISTINCT FROM
          (OLD.principal, OLD.kind, OLD.key_id, OLD.material, OLD.issuer, OLD.audience,
           OLD.not_before, OLD.expires_at, OLD.created_at, OLD.created_by) THEN
        RAISE EXCEPTION 'admin_remote_principals: only revoked_at may be set, once'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_admin_remote_principals_update BEFORE UPDATE ON admin_remote_principals
    FOR EACH ROW EXECUTE FUNCTION admin_remote_principals_revoke_once();
CREATE TRIGGER tr_admin_remote_principals_delete BEFORE DELETE ON admin_remote_principals
    FOR EACH ROW EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_remote_principals_truncate BEFORE TRUNCATE ON admin_remote_principals
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

-- ---- Single-use claims (nonce, jti, approval id) ------------------------------------
CREATE TABLE admin_security_claims (
    kind        VARCHAR(16)  NOT NULL,
    value       VARCHAR(128) NOT NULL,
    keep_until  TIMESTAMPTZ  NOT NULL,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_admin_security_claims PRIMARY KEY (kind, value),
    CONSTRAINT ck_admin_security_claims_kind CHECK (kind IN ('NONCE', 'JTI', 'APPROVAL')),
    CONSTRAINT ck_admin_security_claims_value CHECK (char_length(value) BETWEEN 1 AND 128)
);
CREATE INDEX ix_admin_security_claims_keep_until ON admin_security_claims (keep_until);

-- ---- Revocations --------------------------------------------------------------------
CREATE TABLE admin_revocations (
    id           BIGINT       GENERATED ALWAYS AS IDENTITY,
    sub          VARCHAR(128) NOT NULL,
    session_id   VARCHAR(128),
    revoked_at   TIMESTAMPTZ  NOT NULL,
    reason_code  VARCHAR(64)  NOT NULL,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    created_by   VARCHAR(128) NOT NULL,
    CONSTRAINT pk_admin_revocations PRIMARY KEY (id),
    CONSTRAINT ck_admin_revocations_sub CHECK (sub ~ '^[A-Za-z0-9._:@-]{1,128}$'),
    CONSTRAINT ck_admin_revocations_session CHECK (session_id IS NULL OR char_length(session_id) BETWEEN 1 AND 128),
    CONSTRAINT ck_admin_revocations_reason CHECK (reason_code ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_admin_revocations_created_by CHECK (char_length(btrim(created_by)) BETWEEN 1 AND 128)
);
CREATE INDEX ix_admin_revocations_sub ON admin_revocations (sub) WHERE session_id IS NULL;
CREATE INDEX ix_admin_revocations_session ON admin_revocations (session_id) WHERE session_id IS NOT NULL;
CREATE TRIGGER tr_admin_revocations_update BEFORE UPDATE OR DELETE ON admin_revocations
    FOR EACH ROW EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_revocations_truncate BEFORE TRUNCATE ON admin_revocations
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

-- ---- Rate limits --------------------------------------------------------------------
CREATE TABLE admin_rate_limits (
    bucket        VARCHAR(64) NOT NULL,
    window_start  TIMESTAMPTZ NOT NULL,
    count         INTEGER     NOT NULL,
    CONSTRAINT pk_admin_rate_limits PRIMARY KEY (bucket, window_start),
    CONSTRAINT ck_admin_rate_limits_bucket CHECK (bucket ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_admin_rate_limits_count CHECK (count >= 1)
);
CREATE INDEX ix_admin_rate_limits_window ON admin_rate_limits (window_start);

-- ---- Signed access policies ---------------------------------------------------------
CREATE TABLE admin_policy_revisions (
    revision         BIGINT       NOT NULL,
    document         BYTEA        NOT NULL,
    signature        VARCHAR(128) NOT NULL,
    key_id           VARCHAR(128) NOT NULL,
    document_sha256  VARCHAR(64)  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    created_by       VARCHAR(128) NOT NULL,
    CONSTRAINT pk_admin_policy_revisions PRIMARY KEY (revision),
    CONSTRAINT ck_admin_policy_revisions_revision CHECK (revision >= 1),
    CONSTRAINT ck_admin_policy_revisions_document CHECK (octet_length(document) BETWEEN 2 AND 1048576),
    CONSTRAINT ck_admin_policy_revisions_signature CHECK (signature ~ '^[0-9a-f]{128}$'),
    CONSTRAINT ck_admin_policy_revisions_key_id CHECK (key_id ~ '^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$'),
    CONSTRAINT ck_admin_policy_revisions_digest CHECK (document_sha256 = encode(sha256(document), 'hex')),
    CONSTRAINT ck_admin_policy_revisions_created_by CHECK (char_length(btrim(created_by)) BETWEEN 1 AND 128)
);
CREATE TRIGGER tr_admin_policy_revisions_update BEFORE UPDATE OR DELETE ON admin_policy_revisions
    FOR EACH ROW EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_policy_revisions_truncate BEFORE TRUNCATE ON admin_policy_revisions
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

-- ---- Hash-chained audit -------------------------------------------------------------
CREATE TABLE admin_audit_entries (
    sequence        BIGINT        GENERATED ALWAYS AS IDENTITY,
    entry_hash      VARCHAR(64)   NOT NULL,
    previous_hash   VARCHAR(64)   NOT NULL,
    correlation_id  UUID          NOT NULL,
    occurred_at     TIMESTAMPTZ   NOT NULL,
    actor           VARCHAR(160)  NOT NULL,
    session_id      VARCHAR(128),
    capability      VARCHAR(96),
    method          VARCHAR(8)    NOT NULL,
    path            VARCHAR(4096) NOT NULL,
    purpose         VARCHAR(64),
    scope           JSON          NOT NULL,
    decision        VARCHAR(64)   NOT NULL,
    http_status     INTEGER       NOT NULL,
    request_digest  VARCHAR(64),
    approval_id     VARCHAR(128),
    approver        VARCHAR(160),
    detail          JSON          NOT NULL,
    recorded_at     TIMESTAMPTZ   NOT NULL DEFAULT now(),
    CONSTRAINT pk_admin_audit_entries PRIMARY KEY (sequence),
    CONSTRAINT uq_admin_audit_entries_hash UNIQUE (entry_hash),
    CONSTRAINT ck_admin_audit_entries_hashes CHECK (entry_hash ~ '^[0-9a-f]{64}$' AND previous_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_admin_audit_entries_decision CHECK (decision ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_admin_audit_entries_status CHECK (http_status BETWEEN 100 AND 599),
    CONSTRAINT ck_admin_audit_entries_digest CHECK (request_digest IS NULL OR request_digest ~ '^[0-9a-f]{64}$')
);
CREATE INDEX ix_admin_audit_entries_occurred ON admin_audit_entries (occurred_at);
CREATE INDEX ix_admin_audit_entries_actor ON admin_audit_entries (actor, occurred_at);

CREATE FUNCTION admin_audit_chain_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    last_hash VARCHAR(64);
BEGIN
    PERFORM pg_advisory_xact_lock(782251321260601345);
    SELECT entry_hash INTO last_hash FROM admin_audit_entries ORDER BY sequence DESC LIMIT 1;
    IF NEW.previous_hash IS DISTINCT FROM COALESCE(last_hash, repeat('0', 64)) THEN
        RAISE EXCEPTION 'admin_audit_entries: previous_hash does not continue the chain'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_admin_audit_entries_chain BEFORE INSERT ON admin_audit_entries
    FOR EACH ROW EXECUTE FUNCTION admin_audit_chain_guard();
CREATE TRIGGER tr_admin_audit_entries_update BEFORE UPDATE OR DELETE ON admin_audit_entries
    FOR EACH ROW EXECUTE FUNCTION admin_append_only();
CREATE TRIGGER tr_admin_audit_entries_truncate BEFORE TRUNCATE ON admin_audit_entries
    FOR EACH STATEMENT EXECUTE FUNCTION admin_append_only();

-- ---- Summaries for HQ (counts only) -------------------------------------------------
CREATE TABLE admin_dataset_days (
    dataset_id        VARCHAR(56)  NOT NULL,
    day               DATE         NOT NULL,
    metrics           JSONB        NOT NULL DEFAULT '{}'::jsonb,
    sources_reported  TEXT[]       NOT NULL DEFAULT '{}',
    updated_at        TIMESTAMPTZ  NOT NULL DEFAULT now(),
    CONSTRAINT pk_admin_dataset_days PRIMARY KEY (dataset_id, day),
    CONSTRAINT ck_admin_dataset_days_id CHECK (dataset_id ~ '^cfmeet\.[a-z_]{1,48}$'),
    CONSTRAINT ck_admin_dataset_days_metrics CHECK (jsonb_typeof(metrics) = 'object')
);

CREATE TABLE admin_projections (
    name           VARCHAR(64) NOT NULL,
    last_event_at  TIMESTAMPTZ,
    open_gaps      BIGINT      NOT NULL DEFAULT 0,
    quarantined    BIGINT      NOT NULL DEFAULT 0,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pk_admin_projections PRIMARY KEY (name),
    CONSTRAINT ck_admin_projections_name CHECK (name ~ '^[a-z][a-z0-9_.]{0,63}$'),
    CONSTRAINT ck_admin_projections_counts CHECK (open_gaps >= 0 AND quarantined >= 0)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS admin_projections;
DROP TABLE IF EXISTS admin_dataset_days;
DROP TABLE IF EXISTS admin_audit_entries;
DROP FUNCTION IF EXISTS admin_audit_chain_guard();
DROP TABLE IF EXISTS admin_policy_revisions;
DROP TABLE IF EXISTS admin_rate_limits;
DROP TABLE IF EXISTS admin_revocations;
DROP TABLE IF EXISTS admin_security_claims;
DROP TABLE IF EXISTS admin_remote_principals;
DROP FUNCTION IF EXISTS admin_remote_principals_revoke_once();
DROP FUNCTION IF EXISTS admin_append_only();
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        debug_assert_eq!(AUDIT_LOCK_KEY, 782_251_321_260_601_345);
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
