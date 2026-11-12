//! Final production completion: durable email delivery and conference presence.
//! Additive, own history (`lifecycle_delivery_migrations`, binary
//! `lifecycle-delivery-migration`), so the previously deployed image still starts after it
//! is applied (rollback safety, DATA-011 target rule).
//!
//! * `email_deliveries` — the durable outgoing email queue and delivery ledger (replaces
//!   the volatile Redis `email:queue` list): one row per message, unique idempotency key,
//!   leased by the email worker (`FOR UPDATE SKIP LOCKED`, multi-instance safe), retried
//!   with exponential backoff, status from QUEUED to provider evidence (ACCEPTED,
//!   DELIVERED, BOUNCED, COMPLAINED) or FAILED. Message content (which may carry a
//!   one-time code) is erased once the row reaches a final state.
//! * `email_delivery_events` — provider webhook events, once per provider event id
//!   (duplicate deliveries are acknowledged and ignored).
//! * `conference_presence` — the durable empty-interval tracking of LIVE instant
//!   conferences (10 minutes without anyone present ends the conference; restart safe).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE email_deliveries (
    id                   UUID          NOT NULL,
    category             VARCHAR(48)   NOT NULL,
    idempotency_key      VARCHAR(160)  NOT NULL,
    recipient_hash       CHAR(64)      NOT NULL,
    recipient            TEXT,
    recipient_name       TEXT,
    subject              TEXT          NOT NULL,
    html_body            TEXT,
    text_body            TEXT,
    status               VARCHAR(16)   NOT NULL DEFAULT 'QUEUED',
    provider             VARCHAR(16),
    provider_message_id  VARCHAR(128),
    attempts             INTEGER       NOT NULL DEFAULT 0,
    next_attempt_at      TIMESTAMPTZ   NOT NULL DEFAULT now(),
    lease_holder         VARCHAR(96),
    lease_expires_at     TIMESTAMPTZ,
    last_error_class     VARCHAR(64),
    last_error_status    INTEGER,
    expires_at           TIMESTAMPTZ,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT now(),
    accepted_at          TIMESTAMPTZ,
    delivered_at         TIMESTAMPTZ,
    final_at             TIMESTAMPTZ,
    CONSTRAINT pk_email_deliveries PRIMARY KEY (id),
    CONSTRAINT uq_email_deliveries_idempotency UNIQUE (idempotency_key),
    CONSTRAINT ck_email_deliveries_status CHECK (status IN
        ('QUEUED', 'SENDING', 'ACCEPTED', 'DELIVERED', 'DELAYED', 'BOUNCED', 'COMPLAINED', 'FAILED', 'EXPIRED')),
    CONSTRAINT ck_email_deliveries_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_email_deliveries_category CHECK (category ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE INDEX ix_email_deliveries_due ON email_deliveries (next_attempt_at)
    WHERE status IN ('QUEUED', 'SENDING');
CREATE UNIQUE INDEX uq_email_deliveries_provider_message
    ON email_deliveries (provider, provider_message_id) WHERE provider_message_id IS NOT NULL;
CREATE INDEX ix_email_deliveries_recipient ON email_deliveries (recipient_hash, created_at);
CREATE INDEX ix_email_deliveries_status ON email_deliveries (status, updated_at);

CREATE TABLE email_delivery_events (
    provider             VARCHAR(16)   NOT NULL,
    event_id             VARCHAR(128)  NOT NULL,
    event_type           VARCHAR(48)   NOT NULL,
    provider_message_id  VARCHAR(128),
    delivery_id          UUID,
    occurred_at          TIMESTAMPTZ,
    received_at          TIMESTAMPTZ   NOT NULL DEFAULT now(),
    CONSTRAINT pk_email_delivery_events PRIMARY KEY (provider, event_id)
);
CREATE INDEX ix_email_delivery_events_delivery ON email_delivery_events (delivery_id);

CREATE TABLE conference_presence (
    conference_id        UUID          NOT NULL,
    empty_since          TIMESTAMPTZ,
    last_occupied_at     TIMESTAMPTZ,
    last_checked_at      TIMESTAMPTZ   NOT NULL DEFAULT now(),
    CONSTRAINT pk_conference_presence PRIMARY KEY (conference_id),
    CONSTRAINT fk_conference_presence_conference FOREIGN KEY (conference_id)
        REFERENCES conference_sessions (id) ON DELETE CASCADE
);
CREATE INDEX ix_conference_presence_empty ON conference_presence (empty_since)
    WHERE empty_since IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS conference_presence;
DROP TABLE IF EXISTS email_delivery_events;
DROP TABLE IF EXISTS email_deliveries;
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
