//! "Added to a group" notices for the Notification Service (additive; its own
//! `messaging_member_notifications_seaql_migrations` history, so a previous Messaging image,
//! which fails closed on unknown migrations of the histories it knows, never sees it).
//!
//! `messaging_member_notifications`: one row per person someone else added to (or invited
//! into) a multi-party conversation, written in the membership transaction only while the
//! Notification Service is configured. The relay posts it to I1 as catalogue type #14
//! `messaging.member.invited` (category `GROUP_INVITATIONS`), addressed to the member's CF
//! Meet account. There is no legacy notice for this fact, so there is no routing fence:
//! each row carries its own route decision (`routingVersion` 1) and delivery is governed only
//! by the service's `notif_routing` rows. The envelope is ids only (the CHECK refuses any
//! data key but the catalogue's two, and any sealed part).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_member_notifications (
    id                  UUID         NOT NULL,
    event_id            VARCHAR(64)  NOT NULL,
    outbox_event_id     UUID         NOT NULL,
    decision_id         UUID         NOT NULL,
    tenant_id           UUID         NOT NULL,
    application_id      UUID         NOT NULL,
    environment_id      UUID         NOT NULL,
    conversation_id     UUID         NOT NULL,
    event_sequence      BIGINT       NOT NULL,
    principal_id        UUID         NOT NULL,
    actor_principal_id  UUID,
    service_account_id  UUID         NOT NULL,
    environment_kind    VARCHAR(16)  NOT NULL,
    first_party         BOOLEAN      NOT NULL,
    correlation_id      VARCHAR(37)  NOT NULL,
    envelope            JSONB,
    state               VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    outcome             VARCHAR(9),
    last_error          VARCHAR(64),
    attempts            INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at     TIMESTAMPTZ  NOT NULL,
    occurred_at         TIMESTAMPTZ  NOT NULL,
    created_at          TIMESTAMPTZ  NOT NULL,
    completed_at        TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_member_notifications PRIMARY KEY (id),
    CONSTRAINT uq_messaging_member_notifications_event UNIQUE (event_id),
    CONSTRAINT uq_messaging_member_notifications_decision UNIQUE (decision_id),
    CONSTRAINT ck_messaging_member_notifications_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_member_notifications_sequence CHECK (event_sequence >= 0),
    CONSTRAINT ck_messaging_member_notifications_correlation CHECK (correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_member_notifications_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_messaging_member_notifications_outcome CHECK (outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    CONSTRAINT ck_messaging_member_notifications_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_messaging_member_notifications_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_messaging_member_notifications_done CHECK ((state = 'PENDING') = (completed_at IS NULL)),
    CONSTRAINT ck_messaging_member_notifications_envelope CHECK (envelope IS NULL OR (
        jsonb_typeof(envelope) = 'object'
        AND octet_length(envelope::text) <= 4096
        AND NOT (envelope ? 'sealed')
        AND envelope ->> 'eventType' = 'messaging.member.invited'
        AND jsonb_typeof(envelope -> 'data') = 'object'
        AND (envelope -> 'data') - ARRAY['conversationId', 'eventSequence'] = '{}'::jsonb
        AND envelope ->> 'eventId' = event_id))
);
CREATE INDEX ix_messaging_member_notifications_due ON messaging_member_notifications (next_attempt_at)
    WHERE state = 'PENDING';
CREATE INDEX ix_messaging_member_notifications_settled ON messaging_member_notifications (completed_at)
    WHERE state <> 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_member_notifications;
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
