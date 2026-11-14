//! Account-wide linked devices (multi-device policy): one primary mobile, several web
//! devices, web-only accounts.
//!
//! Appended to the `account_security_migrations` history. Additive only: the previously
//! deployed image keeps starting (its readiness check is "applied ⊇ known") and never
//! reads the new objects. Existing sessions are not touched: they stay unclassified
//! (`device_id IS NULL`), keep working exactly as before and never count against the
//! device limits, so nobody is signed out by this migration.
//!
//! * `account_devices` — one row per logical device (one browser profile or one mobile
//!   app installation), bound to a device-generated Ed25519 public key. The database
//!   enforces at most one ACTIVE (primary) mobile per account and one live row per
//!   installation and per key.
//! * `account_sessions.device_id` — the device a sign-in belongs to (NULL: unclassified).
//! * `account_device_links` — short-lived, one-time linking challenges (approval by a
//!   trusted device or an email step-up), bound to the account and the requesting device.
//! * `account_device_proof_nonces` — single-use proof-of-possession nonces (replay guard).
//! * `account_session_media` — which meeting participant sessions an account session
//!   opened, so ending that sign-in ends only its own meeting media.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE account_devices (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    installation_digest CHAR(64) NOT NULL CHECK (installation_digest ~ '^[0-9a-f]{64}$'),
    device_class VARCHAR(8) NOT NULL CHECK (device_class IN ('WEB', 'MOBILE')),
    platform VARCHAR(8) NOT NULL CHECK (platform IN ('WEB', 'IOS', 'ANDROID')),
    label VARCHAR(80) NULL,
    public_key BYTEA NOT NULL CHECK (octet_length(public_key) = 32),
    key_thumbprint CHAR(64) NOT NULL CHECK (key_thumbprint ~ '^[0-9a-f]{64}$'),
    trust VARCHAR(12) NOT NULL CHECK (trust IN ('UNTRUSTED', 'TRUSTED')),
    trusted_via VARCHAR(16) NULL
        CHECK (trusted_via IS NULL OR trusted_via IN ('BOOTSTRAP', 'DEVICE_APPROVAL', 'EMAIL_STEP_UP')),
    trusted_at TIMESTAMPTZ NULL,
    state VARCHAR(24) NOT NULL CHECK (state IN ('ACTIVE', 'REPLACEMENT_PENDING', 'REVOKED')),
    promoted_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ NULL,
    revoke_reason VARCHAR(32) NULL CHECK (revoke_reason IS NULL OR revoke_reason IN (
        'USER_REVOKED', 'DEVICE_REPLACED', 'LOGOUT_ALL', 'ADMIN_REVOKED', 'REPLACEMENT_ABANDONED'
    )),
    replaced_by_device_id UUID NULL,
    CONSTRAINT ck_account_devices_platform CHECK (
        (device_class = 'WEB' AND platform = 'WEB')
        OR (device_class = 'MOBILE' AND platform IN ('IOS', 'ANDROID'))
    ),
    CONSTRAINT ck_account_devices_revocation CHECK (
        (state = 'REVOKED') = (revoked_at IS NOT NULL)
        AND (revoked_at IS NULL) = (revoke_reason IS NULL)
    ),
    CONSTRAINT ck_account_devices_trust CHECK (
        (trust = 'TRUSTED') = (trusted_via IS NOT NULL AND trusted_at IS NOT NULL)
    ),
    -- Only a mobile waits for replacement; an ACTIVE mobile is the trusted primary.
    CONSTRAINT ck_account_devices_pending_is_mobile CHECK (
        state <> 'REPLACEMENT_PENDING' OR device_class = 'MOBILE'
    ),
    CONSTRAINT ck_account_devices_primary_trusted CHECK (
        NOT (device_class = 'MOBILE' AND state = 'ACTIVE') OR trust = 'TRUSTED'
    ),
    CONSTRAINT ck_account_devices_label CHECK (label IS NULL OR length(label) BETWEEN 1 AND 80)
);
-- The authoritative rule: at most one primary (ACTIVE) mobile per account.
CREATE UNIQUE INDEX uq_account_devices_primary_mobile
    ON account_devices (user_id) WHERE device_class = 'MOBILE' AND state = 'ACTIVE';
-- One live logical device per installation (tabs of one browser profile share it).
CREATE UNIQUE INDEX uq_account_devices_installation
    ON account_devices (user_id, installation_digest) WHERE state <> 'REVOKED';
-- A device key belongs to one live device of one account.
CREATE UNIQUE INDEX uq_account_devices_key
    ON account_devices (key_thumbprint) WHERE state <> 'REVOKED';
CREATE INDEX idx_account_devices_user ON account_devices (user_id, created_at DESC);

CREATE FUNCTION account_devices_guard() RETURNS trigger AS $$
BEGIN
    IF OLD.state = 'REVOKED' THEN
        RAISE EXCEPTION 'account device % is revoked (terminal)', OLD.id;
    END IF;
    IF NEW.user_id <> OLD.user_id OR NEW.public_key <> OLD.public_key
       OR NEW.key_thumbprint <> OLD.key_thumbprint
       OR NEW.installation_digest <> OLD.installation_digest
       OR NEW.device_class <> OLD.device_class THEN
        RAISE EXCEPTION 'account device identity is immutable';
    END IF;
    IF OLD.trust = 'TRUSTED' AND NEW.trust <> 'TRUSTED' THEN
        RAISE EXCEPTION 'device trust is revoked by revoking the device';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER trg_account_devices_guard BEFORE UPDATE ON account_devices
    FOR EACH ROW EXECUTE FUNCTION account_devices_guard();
CREATE FUNCTION account_devices_no_delete() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'account devices are never deleted (audit)';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER trg_account_devices_no_delete BEFORE DELETE ON account_devices
    FOR EACH ROW EXECUTE FUNCTION account_devices_no_delete();

ALTER TABLE account_sessions ADD COLUMN device_id UUID NULL;
CREATE INDEX idx_account_sessions_device ON account_sessions (device_id)
    WHERE device_id IS NOT NULL AND revoked_at IS NULL;
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS account_sessions_revoke_reason_check;
ALTER TABLE account_sessions ADD CONSTRAINT ck_account_sessions_revoke_reason CHECK (
    revoke_reason IS NULL OR revoke_reason IN (
        'LOGOUT', 'LOGOUT_ALL', 'USER_REVOKED', 'REFRESH_REUSE', 'CREDENTIALS_CHANGED',
        'ACCOUNT_DISABLED', 'ADMIN_REVOKED', 'EXPIRED',
        'DEVICE_REPLACED', 'DEVICE_REVOKED', 'SUPERSEDED', 'REPLACEMENT_ABANDONED'
    )
);

CREATE TABLE account_device_links (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    device_id UUID NOT NULL,
    comparison_code CHAR(6) NOT NULL CHECK (comparison_code ~ '^[0-9]{6}$'),
    status VARCHAR(12) NOT NULL
        CHECK (status IN ('PENDING', 'APPROVED', 'COMPLETED', 'CANCELLED', 'EXPIRED', 'REJECTED')),
    method VARCHAR(16) NULL CHECK (method IS NULL OR method IN ('DEVICE_APPROVAL', 'EMAIL_STEP_UP')),
    approved_by_device_id UUID NULL,
    approved_at TIMESTAMPTZ NULL,
    email_code_digest CHAR(64) NULL,
    email_code_sent_at TIMESTAMPTZ NULL,
    email_attempts INTEGER NOT NULL DEFAULT 0 CHECK (email_attempts >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ NULL,
    CONSTRAINT ck_account_device_links_approval CHECK (
        (status IN ('APPROVED', 'COMPLETED')) = (method IS NOT NULL)
    ),
    CONSTRAINT ck_account_device_links_window CHECK (expires_at > created_at)
);
CREATE UNIQUE INDEX uq_account_device_links_open
    ON account_device_links (device_id) WHERE status IN ('PENDING', 'APPROVED');
CREATE INDEX idx_account_device_links_user ON account_device_links (user_id, created_at DESC);

CREATE TABLE account_device_proof_nonces (
    digest CHAR(64) PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX idx_account_device_proof_nonces_expiry ON account_device_proof_nonces (expires_at);

CREATE TABLE account_session_media (
    participant_session_id UUID PRIMARY KEY,
    account_session_id UUID NOT NULL,
    conference_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_account_session_media_session ON account_session_media (account_session_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS account_session_media;
DROP TABLE IF EXISTS account_device_proof_nonces;
DROP TABLE IF EXISTS account_device_links;
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS ck_account_sessions_revoke_reason;
UPDATE account_sessions SET revoke_reason = 'USER_REVOKED'
    WHERE revoke_reason IN ('DEVICE_REPLACED', 'DEVICE_REVOKED', 'SUPERSEDED', 'REPLACEMENT_ABANDONED');
ALTER TABLE account_sessions ADD CONSTRAINT account_sessions_revoke_reason_check CHECK (
    revoke_reason IS NULL OR revoke_reason IN (
        'LOGOUT', 'LOGOUT_ALL', 'USER_REVOKED', 'REFRESH_REUSE', 'CREDENTIALS_CHANGED',
        'ACCOUNT_DISABLED', 'ADMIN_REVOKED', 'EXPIRED'
    )
);
DROP INDEX IF EXISTS idx_account_sessions_device;
ALTER TABLE account_sessions DROP COLUMN IF EXISTS device_id;
DROP TRIGGER IF EXISTS trg_account_devices_no_delete ON account_devices;
DROP FUNCTION IF EXISTS account_devices_no_delete();
DROP TRIGGER IF EXISTS trg_account_devices_guard ON account_devices;
DROP FUNCTION IF EXISTS account_devices_guard();
DROP TABLE IF EXISTS account_devices;
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
