//! Blueprint v1.0 Section 13 — MANAGED realtime session devices and the inbox summary.
//!
//! * `messaging_session_devices` (ADR 0049 amendment A1): a *session device* lets a
//!   first-party client hold a realtime connection for MANAGED conversations without an
//!   MLS device. It is NOT a `messaging_devices` row, so it can never own KeyPackages,
//!   Welcomes, push registrations, history transfers or a place in an MLS group, and it is
//!   never listed or counted as an end-to-end encrypted device. It is bound to the account
//!   session that created it through an opaque, domain-separated SHA-256 reference
//!   supplied by the trusted relay (never reversible to the session id), lives at most one
//!   day and is ACTIVE → REVOKED (terminal).
//! * `messaging_client_sessions.device_kind`: a client session is bound either to an MLS
//!   device (`E2EE_DEVICE`, as before) or to a session device (`SESSION_DEVICE`). The
//!   original foreign key keeps protecting E2EE-device sessions through a generated
//!   column; session-device sessions reference `messaging_session_devices`.
//! * `messaging_conversations.title` / `avatar_ref`: display metadata of MANAGED
//!   conversations (refused for PRIVATE_E2EE, whose metadata belongs to the MLS group).
//! * `messaging_message_mentions`: the principals a MANAGED message mentions (ids only),
//!   for the per-member mention count of the conversation list.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_session_devices (
    id                      UUID         NOT NULL,
    tenant_id               UUID         NOT NULL,
    application_id          UUID         NOT NULL,
    environment_id          UUID         NOT NULL,
    principal_id            UUID         NOT NULL,
    account_session_sha256  CHAR(64)     NOT NULL,
    status                  VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    created_at              TIMESTAMPTZ  NOT NULL,
    expires_at              TIMESTAMPTZ  NOT NULL,
    revoked_at              TIMESTAMPTZ,
    revoked_reason          VARCHAR(48),
    CONSTRAINT pk_messaging_session_devices PRIMARY KEY (id),
    CONSTRAINT ck_messaging_session_devices_status CHECK (status IN ('ACTIVE', 'REVOKED')),
    CONSTRAINT ck_messaging_session_devices_revoked CHECK ((status = 'REVOKED') = (revoked_at IS NOT NULL)),
    CONSTRAINT ck_messaging_session_devices_expiry CHECK (expires_at > created_at AND expires_at <= created_at + INTERVAL '1 day'),
    CONSTRAINT ck_messaging_session_devices_binding CHECK (account_session_sha256 ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_messaging_session_devices_reason CHECK (revoked_reason IS NULL OR revoked_reason ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE INDEX ix_messaging_session_devices_binding
    ON messaging_session_devices (environment_id, principal_id, account_session_sha256) WHERE status = 'ACTIVE';
CREATE FUNCTION messaging_session_devices_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'REVOKED' AND NEW.status <> 'REVOKED' THEN
        RAISE EXCEPTION 'messaging: a revoked session device is terminal' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.principal_id <> OLD.principal_id OR NEW.environment_id <> OLD.environment_id
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.account_session_sha256 <> OLD.account_session_sha256 OR NEW.expires_at > OLD.expires_at THEN
        RAISE EXCEPTION 'messaging: session device identity is immutable' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_session_devices_guard BEFORE UPDATE ON messaging_session_devices
    FOR EACH ROW EXECUTE FUNCTION messaging_session_devices_guard();

ALTER TABLE messaging_client_sessions
    ADD COLUMN device_kind VARCHAR(16) NOT NULL DEFAULT 'E2EE_DEVICE',
    ADD CONSTRAINT ck_messaging_client_sessions_device_kind CHECK (device_kind IN ('E2EE_DEVICE', 'SESSION_DEVICE'));
ALTER TABLE messaging_client_sessions DROP CONSTRAINT fk_messaging_client_sessions_device;
ALTER TABLE messaging_client_sessions
    ADD COLUMN e2ee_device_id UUID GENERATED ALWAYS AS (CASE WHEN device_kind = 'E2EE_DEVICE' THEN device_id END) STORED,
    ADD COLUMN session_device_id UUID GENERATED ALWAYS AS (CASE WHEN device_kind = 'SESSION_DEVICE' THEN device_id END) STORED;
ALTER TABLE messaging_client_sessions
    ADD CONSTRAINT fk_messaging_client_sessions_device FOREIGN KEY (e2ee_device_id) REFERENCES messaging_devices (id),
    ADD CONSTRAINT fk_messaging_client_sessions_session_device FOREIGN KEY (session_device_id) REFERENCES messaging_session_devices (id);
CREATE OR REPLACE FUNCTION messaging_client_sessions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.status = 'REVOKED' AND NEW.status <> 'REVOKED' THEN
        RAISE EXCEPTION 'messaging: a revoked client session is terminal' USING ERRCODE = 'P0001';
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.id <> OLD.id OR NEW.device_id <> OLD.device_id OR NEW.principal_id <> OLD.principal_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.expires_at > OLD.expires_at
       OR NEW.device_kind <> OLD.device_kind) THEN
        RAISE EXCEPTION 'messaging: client session identity is immutable' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;

ALTER TABLE messaging_conversations
    ADD COLUMN title VARCHAR(120),
    ADD COLUMN avatar_ref VARCHAR(256),
    ADD CONSTRAINT ck_messaging_conversations_title CHECK (title IS NULL OR (length(btrim(title)) BETWEEN 1 AND 120)),
    ADD CONSTRAINT ck_messaging_conversations_avatar_ref CHECK (avatar_ref IS NULL OR avatar_ref ~ '^[A-Za-z0-9][A-Za-z0-9._:/-]{0,255}$'),
    ADD CONSTRAINT ck_messaging_conversations_display_managed CHECK ((title IS NULL AND avatar_ref IS NULL) OR security_profile = 'MANAGED');

CREATE TABLE messaging_message_mentions (
    message_id       UUID         NOT NULL,
    conversation_id  UUID         NOT NULL,
    principal_id     UUID         NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_mentions PRIMARY KEY (message_id, principal_id),
    CONSTRAINT fk_messaging_message_mentions_message FOREIGN KEY (message_id) REFERENCES messaging_messages (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_message_mentions_principal ON messaging_message_mentions (conversation_id, principal_id, message_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_message_mentions;
ALTER TABLE messaging_conversations
    DROP CONSTRAINT IF EXISTS ck_messaging_conversations_display_managed,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversations_avatar_ref,
    DROP CONSTRAINT IF EXISTS ck_messaging_conversations_title,
    DROP COLUMN IF EXISTS avatar_ref,
    DROP COLUMN IF EXISTS title;
CREATE OR REPLACE FUNCTION messaging_client_sessions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
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
DELETE FROM messaging_realtime_tickets WHERE session_id IN
    (SELECT id FROM messaging_client_sessions WHERE device_kind = 'SESSION_DEVICE');
DELETE FROM messaging_client_sessions WHERE device_kind = 'SESSION_DEVICE';
ALTER TABLE messaging_client_sessions
    DROP CONSTRAINT IF EXISTS fk_messaging_client_sessions_session_device,
    DROP CONSTRAINT IF EXISTS fk_messaging_client_sessions_device,
    DROP COLUMN IF EXISTS session_device_id,
    DROP COLUMN IF EXISTS e2ee_device_id;
ALTER TABLE messaging_client_sessions
    ADD CONSTRAINT fk_messaging_client_sessions_device FOREIGN KEY (device_id) REFERENCES messaging_devices (id),
    DROP CONSTRAINT IF EXISTS ck_messaging_client_sessions_device_kind,
    DROP COLUMN IF EXISTS device_kind;
DROP TRIGGER IF EXISTS trg_messaging_session_devices_guard ON messaging_session_devices;
DROP FUNCTION IF EXISTS messaging_session_devices_guard();
DROP TABLE IF EXISTS messaging_session_devices;
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
