//! Opt-in protected Messaging installations. No existing identity or session is changed.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE account_messaging_installations (
    environment_id UUID NOT NULL,
    messaging_device_id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_device_id UUID NOT NULL REFERENCES account_devices(id),
    signature_key_sha256 CHAR(64) NOT NULL CHECK (signature_key_sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ NULL,
    PRIMARY KEY (environment_id, messaging_device_id)
);
CREATE INDEX account_messaging_installations_account_device
    ON account_messaging_installations(account_device_id) WHERE revoked_at IS NULL;
CREATE TABLE account_messaging_installation_challenges (
    id UUID PRIMARY KEY,
    environment_id UUID NOT NULL,
    messaging_device_id UUID NOT NULL,
    user_id UUID NOT NULL,
    account_device_id UUID NOT NULL REFERENCES account_devices(id),
    account_session_id UUID NOT NULL,
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 32),
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX account_messaging_installation_challenges_expiry
    ON account_messaging_installation_challenges(expires_at);
CREATE TABLE account_messaging_installation_sessions (
    environment_id UUID NOT NULL,
    messaging_device_id UUID NOT NULL,
    account_session_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(environment_id, messaging_device_id, account_session_id),
    FOREIGN KEY(environment_id, messaging_device_id)
        REFERENCES account_messaging_installations(environment_id, messaging_device_id)
);
CREATE TABLE account_messaging_installation_revocations (
    environment_id UUID NOT NULL,
    messaging_device_id UUID NOT NULL,
    user_id UUID NOT NULL,
    reason VARCHAR(32) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts INTEGER NOT NULL DEFAULT 0,
    done_at TIMESTAMPTZ NULL,
    PRIMARY KEY(environment_id, messaging_device_id)
);
CREATE FUNCTION account_messaging_installation_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.environment_id <> OLD.environment_id OR NEW.messaging_device_id <> OLD.messaging_device_id
       OR NEW.user_id <> OLD.user_id OR NEW.account_device_id <> OLD.account_device_id
       OR NEW.signature_key_sha256 <> OLD.signature_key_sha256
       OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at) THEN
        RAISE EXCEPTION 'Messaging installation identity and revocation are immutable';
    END IF;
    RETURN NEW;
END; $$;
CREATE TRIGGER account_messaging_installation_guard BEFORE UPDATE ON account_messaging_installations
    FOR EACH ROW EXECUTE FUNCTION account_messaging_installation_guard();
-- Revocation is queued in the SAME transaction that terminates the Account device,
-- including a lost installation which has no remaining open sign-ins.
CREATE FUNCTION account_messaging_installation_revoke() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.state = 'REVOKED' AND OLD.state <> 'REVOKED' THEN
        WITH revoked AS (
            UPDATE account_messaging_installations SET revoked_at = now()
            WHERE account_device_id = NEW.id AND revoked_at IS NULL
            RETURNING environment_id, messaging_device_id, user_id
        ) INSERT INTO account_messaging_installation_revocations
            (environment_id, messaging_device_id, user_id, reason)
            SELECT environment_id, messaging_device_id, user_id, COALESCE(NEW.revoke_reason,'DEVICE_REVOKED')
            FROM revoked ON CONFLICT DO NOTHING;
    END IF;
    RETURN NEW;
END; $$;
CREATE TRIGGER account_messaging_installation_revoke AFTER UPDATE ON account_devices
    FOR EACH ROW EXECUTE FUNCTION account_messaging_installation_revoke();
"#).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom("Messaging installation migrations are forward-only; retain identities and revocations on rollback".into()))
    }
}
