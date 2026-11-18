//! CF Meet Platform "Build" (ADR 0062 / 0063, build contract §4–§6): partner session
//! devices, the partner chat baseline, MESSAGE_SENT facts and partner push. Additive only.
//!
//! * `messaging_session_devices.binding_kind`: how a session device is bound. Every
//!   existing row (and every first-party relay device) is `ACCOUNT_SESSION`, unchanged. A
//!   partner device exchanged from a CF Meet user token is `PLATFORM_USER_TOKEN`: its
//!   `account_session_sha256` holds SHA-256("cfmeet-user-binding:v1|env|principal|
//!   installationId") (the same opaque 64-hex column), it records the SDK's
//!   `installation_id` and the user token's minting credential (`crd`) and service account
//!   (`sva`), so revoking the partner's credential ends it. The minting credential may
//!   change on renewal (a partner rotating its API key); the binding never does.
//! * `messaging_application_baselines`: the CHECK is widened with
//!   `CFMEET_PLATFORM_PARTNER` (EXTERNAL tenants while the partner flag is on).
//! * `messaging_message_sent_facts`: one row per APPLICATION message accepted in an
//!   EXTERNAL environment (ids, context class and security profile only; never content,
//!   sender or text), the source of the `MESSAGE_SENT` usage export.
//! * `messaging_partner_push_registrations`: the partner push registrations Messaging
//!   synchronised to the Notification Service (never the handle: it lives only there).
//! * `messaging_partner_push_publications`: metadata-only wake-ups of partner MANAGED
//!   messages waiting for the relay to the Notification Service.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_session_devices
    ADD COLUMN IF NOT EXISTS binding_kind VARCHAR(24) NOT NULL DEFAULT 'ACCOUNT_SESSION',
    ADD COLUMN IF NOT EXISTS installation_id VARCHAR(128),
    ADD COLUMN IF NOT EXISTS minting_credential_id UUID,
    ADD COLUMN IF NOT EXISTS minting_service_account_id UUID;
ALTER TABLE messaging_session_devices
    ADD CONSTRAINT ck_messaging_session_devices_binding_kind
        CHECK (binding_kind IN ('ACCOUNT_SESSION', 'PLATFORM_USER_TOKEN')),
    ADD CONSTRAINT ck_messaging_session_devices_partner_binding
        CHECK ((binding_kind = 'PLATFORM_USER_TOKEN') =
               (installation_id IS NOT NULL AND minting_credential_id IS NOT NULL AND minting_service_account_id IS NOT NULL)),
    ADD CONSTRAINT ck_messaging_session_devices_installation
        CHECK (installation_id IS NULL OR installation_id ~ '^[A-Za-z0-9._-]{8,128}$');
CREATE INDEX IF NOT EXISTS ix_messaging_session_devices_minting_credential
    ON messaging_session_devices (minting_credential_id)
    WHERE status = 'ACTIVE' AND minting_credential_id IS NOT NULL;
CREATE OR REPLACE FUNCTION messaging_session_devices_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'REVOKED' AND NEW.status <> 'REVOKED' THEN
        RAISE EXCEPTION 'messaging: a revoked session device is terminal' USING ERRCODE = 'P0001';
    END IF;
    IF NEW.id <> OLD.id OR NEW.principal_id <> OLD.principal_id OR NEW.environment_id <> OLD.environment_id
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.account_session_sha256 <> OLD.account_session_sha256 OR NEW.expires_at > OLD.expires_at
       OR NEW.binding_kind <> OLD.binding_kind OR NEW.installation_id IS DISTINCT FROM OLD.installation_id THEN
        RAISE EXCEPTION 'messaging: session device identity is immutable' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;

ALTER TABLE messaging_application_baselines
    DROP CONSTRAINT IF EXISTS ck_messaging_application_baselines_name;
ALTER TABLE messaging_application_baselines
    ADD CONSTRAINT ck_messaging_application_baselines_name
        CHECK (baseline IN ('CFMEET_FIRST_PARTY', 'CFMEET_PLATFORM_PARTNER'));

CREATE TABLE messaging_message_sent_facts (
    message_id        UUID         NOT NULL,
    tenant_id         UUID         NOT NULL,
    application_id    UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    environment_kind  VARCHAR(16)  NOT NULL,
    context_class     VARCHAR(32)  NOT NULL,
    security_profile  VARCHAR(16)  NOT NULL,
    occurred_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_sent_facts PRIMARY KEY (message_id),
    CONSTRAINT ck_messaging_message_sent_facts_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_messaging_message_sent_facts_profile CHECK (security_profile IN ('MANAGED', 'PRIVATE_E2EE')),
    CONSTRAINT ck_messaging_message_sent_facts_class CHECK (context_class ~ '^[A-Z][A-Z0-9_]{0,31}$')
);
CREATE INDEX IF NOT EXISTS ix_messaging_message_sent_facts_position
    ON messaging_message_sent_facts (occurred_at, message_id);

CREATE TABLE messaging_partner_push_registrations (
    id               UUID          NOT NULL,
    tenant_id        UUID          NOT NULL,
    application_id   UUID          NOT NULL,
    environment_id   UUID          NOT NULL,
    principal_id     UUID          NOT NULL,
    installation_id  VARCHAR(128)  NOT NULL,
    platform         VARCHAR(16)   NOT NULL,
    status           VARCHAR(16)   NOT NULL,
    created_at       TIMESTAMPTZ   NOT NULL,
    updated_at       TIMESTAMPTZ   NOT NULL,
    CONSTRAINT pk_messaging_partner_push_registrations PRIMARY KEY (id),
    CONSTRAINT uq_messaging_partner_push_registrations
        UNIQUE (environment_id, principal_id, installation_id, platform),
    CONSTRAINT ck_messaging_partner_push_registrations_platform CHECK (platform IN ('APNS', 'APNS_VOIP', 'FCM')),
    CONSTRAINT ck_messaging_partner_push_registrations_status CHECK (status IN ('ACTIVE', 'REMOVED')),
    CONSTRAINT ck_messaging_partner_push_registrations_installation
        CHECK (installation_id ~ '^[A-Za-z0-9._-]{8,128}$')
);
CREATE INDEX IF NOT EXISTS ix_messaging_partner_push_registrations_active
    ON messaging_partner_push_registrations (environment_id, principal_id) WHERE status = 'ACTIVE';

CREATE TABLE messaging_partner_push_publications (
    id                UUID          NOT NULL,
    outbox_event_id   UUID          NOT NULL,
    tenant_id         UUID          NOT NULL,
    application_id    UUID          NOT NULL,
    environment_id    UUID          NOT NULL,
    conversation_id   UUID          NOT NULL,
    event_sequence    BIGINT        NOT NULL,
    sender_principal_id UUID,
    correlation_id    VARCHAR(128)  NOT NULL,
    state             VARCHAR(16)   NOT NULL DEFAULT 'PENDING',
    attempts          INTEGER       NOT NULL DEFAULT 0,
    outcome           VARCHAR(64),
    last_error        VARCHAR(64),
    envelope          JSONB,
    next_attempt_at   TIMESTAMPTZ   NOT NULL,
    occurred_at       TIMESTAMPTZ   NOT NULL,
    created_at        TIMESTAMPTZ   NOT NULL,
    expires_at        TIMESTAMPTZ   NOT NULL,
    completed_at      TIMESTAMPTZ,
    CONSTRAINT pk_messaging_partner_push_publications PRIMARY KEY (id),
    CONSTRAINT uq_messaging_partner_push_publications_event UNIQUE (outbox_event_id),
    CONSTRAINT ck_messaging_partner_push_publications_state
        CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED'))
);
CREATE INDEX IF NOT EXISTS ix_messaging_partner_push_publications_due
    ON messaging_partner_push_publications (next_attempt_at) WHERE state = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_partner_push_publications;
DROP TABLE IF EXISTS messaging_partner_push_registrations;
DROP TABLE IF EXISTS messaging_message_sent_facts;
DELETE FROM messaging_application_baselines WHERE baseline = 'CFMEET_PLATFORM_PARTNER';
ALTER TABLE messaging_application_baselines
    DROP CONSTRAINT IF EXISTS ck_messaging_application_baselines_name;
ALTER TABLE messaging_application_baselines
    ADD CONSTRAINT ck_messaging_application_baselines_name CHECK (baseline IN ('CFMEET_FIRST_PARTY'));
CREATE OR REPLACE FUNCTION messaging_session_devices_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
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
DELETE FROM messaging_realtime_tickets WHERE session_id IN (SELECT c.id FROM messaging_client_sessions c
    JOIN messaging_session_devices d ON d.id = c.session_device_id WHERE d.binding_kind = 'PLATFORM_USER_TOKEN');
DELETE FROM messaging_client_sessions WHERE session_device_id IN
    (SELECT id FROM messaging_session_devices WHERE binding_kind = 'PLATFORM_USER_TOKEN');
DELETE FROM messaging_session_devices WHERE binding_kind = 'PLATFORM_USER_TOKEN';
DROP INDEX IF EXISTS ix_messaging_session_devices_minting_credential;
ALTER TABLE messaging_session_devices
    DROP CONSTRAINT IF EXISTS ck_messaging_session_devices_installation,
    DROP CONSTRAINT IF EXISTS ck_messaging_session_devices_partner_binding,
    DROP CONSTRAINT IF EXISTS ck_messaging_session_devices_binding_kind,
    DROP COLUMN IF EXISTS minting_service_account_id,
    DROP COLUMN IF EXISTS minting_credential_id,
    DROP COLUMN IF EXISTS installation_id,
    DROP COLUMN IF EXISTS binding_kind;
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
