//! Realtime messaging (Part V Part E, §58–§68).
//!
//! * `messaging_client_sessions` — device sessions behind short-lived Messaging Client
//!   Access Tokens: bound to tenant / application / environment / principal / device and
//!   the issuing credential; ACTIVE → REVOKED (terminal) or expiry. Every realtime
//!   connection is re-validated against its session.
//! * `messaging_realtime_tickets` — single-use, short-lived WebSocket connection tickets
//!   (only their SHA-256 is stored), so no bearer token ever travels in a URL.
//! * `messaging_presence_settings` / `messaging_presence_last_seen` — the user's presence
//!   visibility and (only when shared and allowed) last-seen time. Live presence itself is
//!   EPHEMERAL (Redis / memory with TTL) and never stored here.
//! * `messaging_push_registrations` / `messaging_push_jobs` — the notification dispatch
//!   boundary: per-device registration and preference, idempotent jobs (one per event and
//!   device) with expiring retries. Payloads never hold message content.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_client_sessions (
    id                    UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    environment_kind      VARCHAR(16)  NOT NULL,
    service_account_id    UUID         NOT NULL,
    principal_id          UUID         NOT NULL,
    device_id             UUID         NOT NULL,
    credential_id         UUID         NOT NULL,
    scopes                VARCHAR(160) NOT NULL,
    status                VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    created_at            TIMESTAMPTZ  NOT NULL,
    expires_at            TIMESTAMPTZ  NOT NULL,
    revoked_at            TIMESTAMPTZ,
    revoked_reason        VARCHAR(48),
    CONSTRAINT pk_messaging_client_sessions PRIMARY KEY (id),
    CONSTRAINT fk_messaging_client_sessions_device FOREIGN KEY (device_id) REFERENCES messaging_devices (id),
    CONSTRAINT ck_messaging_client_sessions_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_messaging_client_sessions_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_messaging_client_sessions_expiry CHECK (expires_at > created_at AND expires_at <= created_at + INTERVAL '1 day'),
    CONSTRAINT ck_messaging_client_sessions_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_messaging_client_sessions_reason CHECK (revoked_reason IS NULL OR revoked_reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE INDEX ix_messaging_client_sessions_device ON messaging_client_sessions (device_id) WHERE status = 'ACTIVE';
CREATE INDEX ix_messaging_client_sessions_expiry ON messaging_client_sessions (expires_at);
CREATE FUNCTION messaging_client_sessions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.status = 'REVOKED' AND NEW.status <> 'REVOKED' THEN
        RAISE EXCEPTION 'messaging: a revoked client session is terminal' USING ERRCODE = 'P0001';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.id <> OLD.id OR NEW.device_id <> OLD.device_id OR NEW.principal_id <> OLD.principal_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.expires_at > OLD.expires_at) THEN
        RAISE EXCEPTION 'messaging: client session identity is immutable' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_client_sessions_guard BEFORE UPDATE ON messaging_client_sessions
    FOR EACH ROW EXECUTE FUNCTION messaging_client_sessions_guard();

CREATE TABLE messaging_realtime_tickets (
    id              UUID         NOT NULL,
    ticket_sha256   CHAR(64)     NOT NULL,
    session_id      UUID         NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    expires_at      TIMESTAMPTZ  NOT NULL,
    consumed_at     TIMESTAMPTZ,
    CONSTRAINT pk_messaging_realtime_tickets PRIMARY KEY (id),
    CONSTRAINT uq_messaging_realtime_tickets_hash UNIQUE (ticket_sha256),
    CONSTRAINT fk_messaging_realtime_tickets_session FOREIGN KEY (session_id) REFERENCES messaging_client_sessions (id),
    CONSTRAINT ck_messaging_realtime_tickets_hash CHECK (ticket_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_realtime_tickets_expiry CHECK (expires_at > created_at AND expires_at <= created_at + INTERVAL '5 minutes')
);
CREATE INDEX ix_messaging_realtime_tickets_expiry ON messaging_realtime_tickets (expires_at);

CREATE TABLE messaging_presence_settings (
    environment_id   UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    visibility       VARCHAR(16)  NOT NULL,
    share_last_seen  BOOLEAN      NOT NULL DEFAULT FALSE,
    updated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_presence_settings PRIMARY KEY (environment_id, principal_id),
    CONSTRAINT ck_messaging_presence_settings_visibility CHECK (visibility IN ('CONVERSATION_MEMBERS', 'HIDDEN'))
);

CREATE TABLE messaging_presence_last_seen (
    environment_id   UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    last_seen_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_presence_last_seen PRIMARY KEY (environment_id, principal_id)
);

CREATE TABLE messaging_push_registrations (
    id               UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    device_id        UUID         NOT NULL,
    platform         VARCHAR(16)  NOT NULL,
    push_handle      VARCHAR(4096) NOT NULL,
    enabled          BOOLEAN      NOT NULL DEFAULT TRUE,
    disabled_reason  VARCHAR(48),
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_push_registrations PRIMARY KEY (id),
    CONSTRAINT uq_messaging_push_registrations_device UNIQUE (device_id),
    CONSTRAINT fk_messaging_push_registrations_device FOREIGN KEY (device_id) REFERENCES messaging_devices (id),
    CONSTRAINT ck_messaging_push_registrations_platform CHECK (platform IN ('APNS', 'FCM', 'WEB_PUSH', 'LOCAL_TEST')),
    CONSTRAINT ck_messaging_push_registrations_handle CHECK (length(push_handle) BETWEEN 8 AND 4096 AND push_handle !~ '[[:cntrl:]]'),
    CONSTRAINT ck_messaging_push_registrations_reason CHECK (disabled_reason IS NULL OR disabled_reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);

CREATE TABLE messaging_push_jobs (
    id               UUID         NOT NULL,
    event_id         UUID         NOT NULL,
    conversation_id  UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    device_id        UUID         NOT NULL,
    registration_id  UUID         NOT NULL,
    payload          JSONB        NOT NULL,
    status           VARCHAR(24)  NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    expires_at       TIMESTAMPTZ  NOT NULL,
    lease_token      UUID,
    lease_expires_at TIMESTAMPTZ,
    last_error       VARCHAR(200),
    created_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_messaging_push_jobs PRIMARY KEY (id),
    CONSTRAINT uq_messaging_push_jobs_event_device UNIQUE (event_id, device_id),
    CONSTRAINT fk_messaging_push_jobs_registration FOREIGN KEY (registration_id) REFERENCES messaging_push_registrations (id),
    CONSTRAINT ck_messaging_push_jobs_status CHECK (status IN ('PENDING', 'SENT', 'FAILED', 'EXPIRED', 'DISABLED', 'NOT_CONFIGURED')),
    CONSTRAINT ck_messaging_push_jobs_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_messaging_push_jobs_lease CHECK ((lease_token IS NULL) = (lease_expires_at IS NULL)),
    -- A push payload is metadata only: never message content, ciphertext or keys.
    CONSTRAINT ck_messaging_push_jobs_payload CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 1024
        AND payload::text !~* '"[a-zA-Z_]*(secret|private|token|ciphertext|plaintext|content|body|text|key)[a-zA-Z_]*":')
);
CREATE INDEX ix_messaging_push_jobs_due ON messaging_push_jobs (next_attempt_at) WHERE status = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_push_jobs;
DROP TABLE IF EXISTS messaging_push_registrations;
DROP TABLE IF EXISTS messaging_presence_last_seen;
DROP TABLE IF EXISTS messaging_presence_settings;
DROP TABLE IF EXISTS messaging_realtime_tickets;
DROP TABLE IF EXISTS messaging_client_sessions;
DROP FUNCTION IF EXISTS messaging_client_sessions_guard();
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
