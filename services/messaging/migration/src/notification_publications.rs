//! Routing fence protocol v1, second migration of the routing fence history
//! ([`crate::NotificationFenceMigrator`]; Notification Service specification, Document 4
//! §9.3, §9.5; roadmap N7): Messaging's own outbox of publications to the Notification
//! Service.
//!
//! * `messaging_notification_publications` — one row per routed message wake-up that the
//!   Notification Service must hear about, written in the push fan-out transaction that
//!   records the routing decision:
//!   - `shadow = TRUE`: the SHADOW copy of an `OLD` decision (the legacy push jobs are
//!     written as well); posted to I1 with `shadow: true`, recorded by the service and
//!     never delivered;
//!   - `shadow = FALSE`: the publication of a `NEW` decision (no legacy push jobs).
//!
//!   At most one row per decision and per envelope event id (the routing key,
//!   `evt_<32 hex>`). The relay builds the `cfmeet.notifications.event` envelope once
//!   (`envelope`, ids only: never content, ciphertext or keys — the CHECK below refuses any
//!   other data key and any sealed part) and re-posts exactly those bytes until I1 answers;
//!   `ACCEPTED`, `DUPLICATE` and `HELD` acknowledge the row (`ACKED`), a refusal dead-letters
//!   it (`DEAD`), an envelope that cannot be built (no CF Meet account recipient, too many
//!   recipients) is `SKIPPED`, and a row past its expiry is `EXPIRED`.
//!
//! * The route decision `reason` CHECK gains `RECIPIENT_LIMIT`: an event whose entitled
//!   audience is larger than one envelope may carry (200 recipients, the I1 contract's
//!   `maxItems`) is decided `OLD` even when the canary would pick `NEW`, so the legacy push
//!   path keeps delivering it; the reason records why.
//!
//! Additive and forward-only; inert while every category is `LEGACY_ONLY`.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261205_000002_notification_publications_v1"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "the notification publication outbox is additive and forward-only".into(),
        ))
    }
}

const UP: &str = r#"
CREATE TABLE messaging_notification_publications (
    id               UUID         NOT NULL,
    event_id         VARCHAR(64)  NOT NULL,
    outbox_event_id  UUID         NOT NULL,
    decision_id      UUID         NOT NULL,
    routing_version  INTEGER      NOT NULL,
    category         VARCHAR(40)  NOT NULL,
    event_type       VARCHAR(64)  NOT NULL,
    shadow           BOOLEAN      NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    conversation_id  UUID         NOT NULL,
    event_sequence   BIGINT       NOT NULL,
    correlation_id   VARCHAR(37)  NOT NULL,
    envelope         JSONB,
    state            VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    outcome          VARCHAR(9),
    last_error       VARCHAR(64),
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    expires_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_notification_publications PRIMARY KEY (id),
    CONSTRAINT uq_messaging_notification_publications_event UNIQUE (event_id),
    CONSTRAINT uq_messaging_notification_publications_decision UNIQUE (decision_id),
    CONSTRAINT fk_messaging_notification_publications_decision FOREIGN KEY (decision_id)
        REFERENCES messaging_notification_route_decisions (decision_id) ON DELETE CASCADE,
    CONSTRAINT ck_messaging_notification_publications_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_notification_publications_category CHECK (category ~ '^[A-Z][A-Z0-9_]{1,39}$'),
    CONSTRAINT ck_messaging_notification_publications_type CHECK (event_type = 'messaging.message.created'),
    CONSTRAINT ck_messaging_notification_publications_version CHECK (routing_version >= 1 AND event_sequence >= 1),
    CONSTRAINT ck_messaging_notification_publications_correlation CHECK (correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_messaging_notification_publications_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_messaging_notification_publications_outcome CHECK (outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    CONSTRAINT ck_messaging_notification_publications_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_messaging_notification_publications_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_messaging_notification_publications_done CHECK ((state = 'PENDING') = (completed_at IS NULL)),
    -- The envelope is ids only: the catalogue's four data keys of messaging.message.created,
    -- never a sealed part, never content, ciphertext or keys.
    CONSTRAINT ck_messaging_notification_publications_envelope CHECK (envelope IS NULL OR (
        jsonb_typeof(envelope) = 'object'
        AND octet_length(envelope::text) <= 8192
        AND NOT (envelope ? 'sealed')
        AND jsonb_typeof(envelope -> 'data') = 'object'
        AND (envelope -> 'data') - ARRAY['conversationId', 'conversationKind', 'eventSequence', 'securityProfile'] = '{}'::jsonb
        AND envelope ->> 'eventId' = event_id))
);
ALTER TABLE messaging_notification_route_decisions
    DROP CONSTRAINT ck_messaging_notification_route_decisions_reason,
    ADD CONSTRAINT ck_messaging_notification_route_decisions_reason CHECK (reason IN ('ROUTED', 'REROUTED_UNACKED',
        'REROUTED_CANCELLED', 'REROUTED_CRITICAL_LEASE', 'AUTO_FAILOVER', 'REROUTED_POSSIBLE_DUPLICATE', 'RECIPIENT_LIMIT'));
CREATE INDEX ix_messaging_notification_publications_due ON messaging_notification_publications (next_attempt_at)
    WHERE state = 'PENDING';
"#;
