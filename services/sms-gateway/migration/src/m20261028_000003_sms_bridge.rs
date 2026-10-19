//! Part V H §§106–112 — the SMS Bridge between Messaging and the SMS Gateway.
//!
//! The SMS Gateway stays the only SMS transport authority: it holds the external party's
//! number (sealed, fingerprinted, masked — Messaging never stores it), applies every
//! transport control (capability via Numbering, opt-out, rate windows, spend, provider)
//! on each bridged send, and owns carrier state and cost evidence.
//!
//! * `sms_bridge_bindings` — one Messaging bridge ↔ (number resource, external party).
//! * `sms_bridge_deliveries` — durable push of each inbound SMS of a bound party to
//!   Messaging (one per transport; leased; retried; dead-lettered).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE sms_bridge_bindings (
    id                  UUID         PRIMARY KEY,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    environment_kind    VARCHAR(16)  NOT NULL,
    service_account_id  UUID         NOT NULL,
    number_resource_id  UUID         NOT NULL,
    remote_sealed       TEXT         NOT NULL,
    remote_fingerprint  VARCHAR(128) NOT NULL,
    remote_masked       VARCHAR(32)  NOT NULL,
    bridge_ref          UUID         NOT NULL,
    status              VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    created_at          TIMESTAMPTZ  NOT NULL,
    closed_at           TIMESTAMPTZ,
    CONSTRAINT uq_sms_bridge_bindings_ref UNIQUE (environment_id, bridge_ref),
    CONSTRAINT ck_sms_bridge_bindings_status CHECK (status IN ('ACTIVE', 'CLOSED')),
    CONSTRAINT ck_sms_bridge_bindings_closed CHECK ((status = 'CLOSED') = (closed_at IS NOT NULL)),
    CONSTRAINT ck_sms_bridge_bindings_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION'))
);
-- One ACTIVE bridge per (number, external party): inbound routing is unambiguous.
CREATE UNIQUE INDEX uq_sms_bridge_bindings_active ON sms_bridge_bindings (environment_id, number_resource_id, remote_fingerprint)
    WHERE status = 'ACTIVE';

CREATE TABLE sms_bridge_deliveries (
    id               UUID         PRIMARY KEY,
    binding_id       UUID         NOT NULL REFERENCES sms_bridge_bindings (id),
    transport_id     UUID         NOT NULL UNIQUE REFERENCES sms_transports (id),
    status           VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    attempt_count    INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_owner      VARCHAR(80),
    lease_expires_at TIMESTAMPTZ,
    last_error       VARCHAR(32),
    created_at       TIMESTAMPTZ  NOT NULL,
    delivered_at     TIMESTAMPTZ,
    CONSTRAINT ck_sms_bridge_deliveries_status CHECK (status IN ('PENDING', 'DELIVERED', 'DEAD_LETTER')),
    CONSTRAINT ck_sms_bridge_deliveries_attempts CHECK (attempt_count >= 0)
);
CREATE INDEX ix_sms_bridge_deliveries_due ON sms_bridge_deliveries (next_attempt_at) WHERE status = 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS sms_bridge_deliveries;
DROP TABLE IF EXISTS sms_bridge_bindings;
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
