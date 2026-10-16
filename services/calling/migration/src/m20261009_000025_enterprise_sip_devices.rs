//! Enterprise SIP devices (Part F §54, §57, §179): desk phones / softphones registered on
//! CF Meet's own SIP / media edge, dialling extensions over ENTERPRISE_INTERNAL.
//!
//! * `enterprise_sip_devices` — binds an opaque Telephony device reference (minted by the
//!   operator in Telephony, where the edge endpoint lives) to a principal of the scope.
//!   Calling never stores a SIP URI, credential, address or edge endpoint name; Telephony
//!   never learns the principal.
//! * `calls.origin` gains ENTERPRISE_DEVICE: a Call offered by Telephony for an internal
//!   SIP dialog (one canonical Call; the device's dialog is a PRIVATE_NETWORK leg).
//! * `call_telephony_commands` gains ANSWER and PROMPT (built-in prompt playback) — the
//!   neutral execution primitives an IVR needs on a SIP leg.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE enterprise_sip_devices (
    id             UUID         NOT NULL,
    tenant_id      UUID         NOT NULL,
    application_id UUID         NOT NULL,
    environment_id UUID         NOT NULL,
    principal_id   UUID         NOT NULL,
    device_ref     VARCHAR(64)  NOT NULL,
    display_label  VARCHAR(100) NOT NULL,
    status         TEXT         NOT NULL,
    version        BIGINT       NOT NULL,
    created_at     TIMESTAMPTZ  NOT NULL,
    updated_at     TIMESTAMPTZ  NOT NULL,
    disabled_at    TIMESTAMPTZ,
    CONSTRAINT pk_enterprise_sip_devices PRIMARY KEY (id),
    CONSTRAINT uq_enterprise_sip_devices_owner_id
        UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_enterprise_sip_devices_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    -- An opaque Telephony reference: never a SIP URI, address or number.
    CONSTRAINT ck_enterprise_sip_devices_ref CHECK (device_ref ~ '^dev-[a-z0-9-]{4,60}$'),
    CONSTRAINT ck_enterprise_sip_devices_label CHECK (length(btrim(display_label)) BETWEEN 1 AND 100),
    CONSTRAINT ck_enterprise_sip_devices_status CHECK (status IN ('ACTIVE', 'DISABLED')),
    CONSTRAINT ck_enterprise_sip_devices_disabled
        CHECK ((status = 'DISABLED') = (disabled_at IS NOT NULL)),
    CONSTRAINT ck_enterprise_sip_devices_version CHECK (version >= 1),
    CONSTRAINT ck_enterprise_sip_devices_timestamps CHECK (updated_at >= created_at)
);
-- A device reference is bound to at most one ACTIVE principal at a time.
CREATE UNIQUE INDEX uq_enterprise_sip_devices_active_ref
    ON enterprise_sip_devices (device_ref) WHERE status = 'ACTIVE';
CREATE INDEX idx_enterprise_sip_devices_scope
    ON enterprise_sip_devices (tenant_id, application_id, environment_id, created_at, id);
CREATE TRIGGER trg_enterprise_sip_devices_immutable
    BEFORE UPDATE ON enterprise_sip_devices
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'device_ref', 'created_at');
CREATE TRIGGER trg_enterprise_sip_devices_version
    BEFORE UPDATE ON enterprise_sip_devices
    FOR EACH ROW EXECUTE FUNCTION enterprise_guard_version();

ALTER TABLE calls
    DROP CONSTRAINT ck_calls_origin,
    ADD CONSTRAINT ck_calls_origin CHECK (origin IN ('APPLICATION', 'PUBLIC_INBOUND', 'ENTERPRISE_DEVICE'));

ALTER TABLE call_telephony_commands
    DROP CONSTRAINT ck_call_telephony_commands_kind,
    DROP CONSTRAINT ck_call_telephony_commands_target,
    ADD CONSTRAINT ck_call_telephony_commands_kind CHECK (kind IN (
        'ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE', 'ANSWER', 'PROMPT')),
    ADD CONSTRAINT ck_call_telephony_commands_target CHECK (
        (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'ANSWER', 'PROMPT') AND call_leg_id IS NOT NULL)
        OR (kind IN ('BRIDGE', 'UNBRIDGE') AND bridge_id IS NOT NULL));
"#;

const DOWN: &str = r#"
ALTER TABLE call_telephony_commands
    DROP CONSTRAINT ck_call_telephony_commands_kind,
    DROP CONSTRAINT ck_call_telephony_commands_target,
    ADD CONSTRAINT ck_call_telephony_commands_kind
        CHECK (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME', 'BRIDGE', 'UNBRIDGE')),
    ADD CONSTRAINT ck_call_telephony_commands_target CHECK (
        (kind IN ('ORIGINATE', 'TERMINATE', 'HOLD', 'RESUME') AND call_leg_id IS NOT NULL)
        OR (kind IN ('BRIDGE', 'UNBRIDGE') AND bridge_id IS NOT NULL));
ALTER TABLE calls
    DROP CONSTRAINT ck_calls_origin,
    ADD CONSTRAINT ck_calls_origin CHECK (origin IN ('APPLICATION', 'PUBLIC_INBOUND'));
DROP TABLE IF EXISTS enterprise_sip_devices;
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
