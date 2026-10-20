//! Part V I §§116–118 — customer webhooks through the EXISTING platform webhook system.
//!
//! Messaging has no webhook subscriptions, secrets or deliveries of its own. Public-worthy
//! committed events are relayed to Calling's platform-event ingress
//! (`/internal/v1/platform/events`, `platform-event.publish`), which projects, signs and
//! delivers them exactly like Calling, Privacy Relay and SMS Gateway events.
//!
//! `messaging_webhook_relay` is the durable relay queue: a trigger on the outbox inserts a
//! row IN THE SAME TRANSACTION as the event for the public event types only
//! (`message.created` only for APPLICATION messages), so nothing is lost and nothing else
//! is queued. The row references the outbox event (whose payload is already ids, states
//! and versions only); it holds no copy of any payload.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_webhook_relay (
    event_id         UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    status           VARCHAR(16)  NOT NULL DEFAULT 'PENDING',
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    last_error       VARCHAR(64),
    created_at       TIMESTAMPTZ  NOT NULL,
    relayed_at       TIMESTAMPTZ,
    CONSTRAINT pk_messaging_webhook_relay PRIMARY KEY (event_id),
    CONSTRAINT fk_messaging_webhook_relay_event FOREIGN KEY (event_id) REFERENCES messaging_outbox_events (id),
    CONSTRAINT ck_messaging_webhook_relay_status CHECK (status IN ('PENDING', 'RELAYED', 'DEAD_LETTER')),
    CONSTRAINT ck_messaging_webhook_relay_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
CREATE INDEX ix_messaging_webhook_relay_due ON messaging_webhook_relay (next_attempt_at) WHERE status = 'PENDING';

CREATE FUNCTION messaging_webhook_relay_enqueue() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.event_type IN ('messaging.conversation.created', 'messaging.conversation.lifecycle-changed',
                          'messaging.member.added', 'messaging.member.removed',
                          'messaging.message.deleted', 'messaging.message.moderated', 'messaging.message.expired')
       OR (NEW.event_type = 'messaging.message.created' AND NEW.payload->>'message_kind' = 'APPLICATION') THEN
        INSERT INTO messaging_webhook_relay (event_id, environment_id, next_attempt_at, created_at)
        VALUES (NEW.id, NEW.environment_id, NEW.occurred_at, NEW.occurred_at);
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_webhook_relay_enqueue AFTER INSERT ON messaging_outbox_events
    FOR EACH ROW EXECUTE FUNCTION messaging_webhook_relay_enqueue();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_messaging_webhook_relay_enqueue ON messaging_outbox_events;
DROP FUNCTION IF EXISTS messaging_webhook_relay_enqueue();
DROP TABLE IF EXISTS messaging_webhook_relay;
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
