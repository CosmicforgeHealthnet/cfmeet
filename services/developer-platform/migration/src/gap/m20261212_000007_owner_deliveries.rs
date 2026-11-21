//! ADR 0068, phase A4 — owner-delivered secrets (owner decision with HQ, 2026-11-21).
//! Additive only.
//!
//! `devplat_owner_deliveries`: one row per secret the Developer Platform delivers DIRECTLY
//! to its rightful recipient on a command relayed by the HQ admin bridge. Today: the
//! one-time OWNER invitation of a first-party workspace (`CLAIM_LINK_EMAIL`), sent through
//! the Notification Service (event #41) to the invited address only, as a single-use link
//! that expires with the invitation. HQ sees a reference only (`id` = `deliveryRef`, the
//! masked address, the state and the expiry), never the token, the link or the address.
//!
//! * The invitation token is stored nowhere in clear: the invitation keeps its SHA-256
//!   digest (single use), and `envelope` holds the event built once at the command, its
//!   token and address SEALED (HPKE) to the Notification Service's key, which only that
//!   service opens. `envelope` exists only while the row is `PENDING`: it is cleared when
//!   the Notification Service accepts or refuses it, or when the link expires unsent.
//! * `recipient_masked` is `a***@e***.com` (or `***`): never the address.
//! * Settled rows are purged 30 days after they settle (the worker).
//!
//! Rollback: DOWN drops the table (inert while the bridge never registers a workspace).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_owner_deliveries (
    id               UUID         NOT NULL,
    admin_key        UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    invitation_id    UUID         NOT NULL,
    channel          VARCHAR(24)  NOT NULL,
    recipient_masked VARCHAR(120) NOT NULL,
    delivery_state   VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    event_id         VARCHAR(36)  NOT NULL,
    envelope         JSONB,
    attempts         INTEGER      NOT NULL DEFAULT 0,
    last_error       VARCHAR(64),
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    expires_at       TIMESTAMPTZ  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    sent_at          TIMESTAMPTZ,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_devplat_owner_deliveries PRIMARY KEY (id),
    CONSTRAINT uq_devplat_owner_deliveries_key UNIQUE (admin_key),
    CONSTRAINT uq_devplat_owner_deliveries_invitation UNIQUE (invitation_id),
    CONSTRAINT uq_devplat_owner_deliveries_event UNIQUE (event_id),
    CONSTRAINT fk_devplat_owner_deliveries_invitation FOREIGN KEY (invitation_id)
        REFERENCES devplat_invitations (id),
    CONSTRAINT ck_devplat_owner_deliveries_channel CHECK (channel IN ('CLAIM_LINK_EMAIL')),
    CONSTRAINT ck_devplat_owner_deliveries_masked CHECK (
        recipient_masked = '***' OR recipient_masked ~ '^[^@\s*]\*\*\*@[^@\s*.]\*\*\*(\.[^@\s*.]{1,63})?$'),
    CONSTRAINT ck_devplat_owner_deliveries_state CHECK (delivery_state IN ('PENDING', 'SENT', 'FAILED', 'EXPIRED')),
    CONSTRAINT ck_devplat_owner_deliveries_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_devplat_owner_deliveries_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_owner_deliveries_attempts CHECK (attempts >= 0),
    -- The sealed envelope lives only while the delivery is pending.
    CONSTRAINT ck_devplat_owner_deliveries_envelope CHECK (
        (delivery_state = 'PENDING') = (envelope IS NOT NULL)
        AND (envelope IS NULL OR (jsonb_typeof(envelope) = 'object' AND octet_length(envelope::text) <= 16384
             AND envelope ->> 'eventId' = event_id))),
    CONSTRAINT ck_devplat_owner_deliveries_done CHECK ((delivery_state = 'PENDING') = (completed_at IS NULL)),
    CONSTRAINT ck_devplat_owner_deliveries_sent CHECK ((delivery_state = 'SENT') = (sent_at IS NOT NULL))
);
CREATE INDEX ix_devplat_owner_deliveries_due ON devplat_owner_deliveries (next_attempt_at)
    WHERE delivery_state = 'PENDING';
CREATE INDEX ix_devplat_owner_deliveries_settled ON devplat_owner_deliveries (completed_at)
    WHERE delivery_state <> 'PENDING';
CREATE INDEX ix_devplat_owner_deliveries_tenant ON devplat_owner_deliveries (tenant_id);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_owner_deliveries;
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
