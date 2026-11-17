//! Notification Service producer outbox (roadmap N7 / N11, Document 4 §5.1, §9.5).
//!
//! * `devplat_notification_publications`: one row per Developer Platform event the
//!   Notification Service must hear about (`developer-platform.credential.expiring-soon`,
//!   `developer-platform.credential.expired`, `developer-platform.production-access.decided`).
//!   It is written in the transaction of the business fact (the operator decision) or by
//!   the worker's credential sweep, only while publication to the Notification Service is
//!   configured. The event id is DETERMINISTIC (`evt_<32 hex>` derived from the fact), so a
//!   fact is enqueued at most once (`UNIQUE (event_id)`), and so is its routing decision
//!   (`decision_id`, derived from the event id).
//!
//!   The relay builds the `cfmeet.notifications.event` envelope once (`envelope`, ids only:
//!   the CHECK below refuses any data key outside the catalogue's and any sealed part) and
//!   re-posts exactly those bytes until I1 answers; `ACCEPTED`, `DUPLICATE` and `HELD`
//!   acknowledge the row (`ACKED`), a refusal dead-letters it (`DEAD`), an envelope without
//!   recipient is `SKIPPED` (reason recorded) and a row past its expiry is `EXPIRED`.
//!   Settled rows are purged after a retention period.
//!
//!   It holds opaque Platform Access ids (tenant, application, environment), the subject
//!   id (credential or production-access request), the deterministic keys and the ids-only
//!   envelope (CF Meet account references of the recipients) — never a credential secret,
//!   prefix, verifier, scope, address or content.
//!
//! Additive and rollback-safe (the down migration drops only what this one created);
//! inert while `CF_NOTIFICATIONS_BASE_URL` is unset.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_notification_publications (
    id               UUID         NOT NULL,
    event_id         VARCHAR(36)  NOT NULL,
    decision_id      UUID         NOT NULL,
    event_type       VARCHAR(64)  NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID,
    subject_id       UUID         NOT NULL,
    data             JSONB        NOT NULL,
    dedupe_key       VARCHAR(200) NOT NULL,
    idempotency_key  VARCHAR(160),
    correlation_id   VARCHAR(37)  NOT NULL,
    envelope         JSONB,
    recipients       INTEGER,
    state            VARCHAR(8)   NOT NULL DEFAULT 'PENDING',
    outcome          VARCHAR(9),
    last_error       VARCHAR(64),
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    created_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    expires_at       TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_notification_publications PRIMARY KEY (id),
    CONSTRAINT uq_devplat_notification_publications_event UNIQUE (event_id),
    CONSTRAINT uq_devplat_notification_publications_decision UNIQUE (decision_id),
    CONSTRAINT ck_devplat_notification_publications_event CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_devplat_notification_publications_type CHECK (event_type IN (
        'developer-platform.credential.expiring-soon',
        'developer-platform.credential.expired',
        'developer-platform.production-access.decided')),
    -- Credential notices are environment-scoped; production-access decisions are not.
    CONSTRAINT ck_devplat_notification_publications_scope CHECK (
        (event_type = 'developer-platform.production-access.decided') = (environment_id IS NULL)),
    CONSTRAINT ck_devplat_notification_publications_data CHECK (
        jsonb_typeof(data) = 'object'
        AND octet_length(data::text) <= 1024
        AND data - ARRAY['applicationId', 'credentialId', 'expiresAt', 'decision', 'requestId'] = '{}'::jsonb),
    CONSTRAINT ck_devplat_notification_publications_dedupe CHECK (dedupe_key ~ '^[\x21-\x7e]{1,200}$'),
    CONSTRAINT ck_devplat_notification_publications_idempotency CHECK (
        idempotency_key IS NULL OR idempotency_key ~ '^[\x21-\x7e]{1,160}$'),
    CONSTRAINT ck_devplat_notification_publications_correlation CHECK (correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_devplat_notification_publications_recipients CHECK (recipients IS NULL OR recipients BETWEEN 1 AND 200),
    CONSTRAINT ck_devplat_notification_publications_state CHECK (state IN ('PENDING', 'ACKED', 'DEAD', 'SKIPPED', 'EXPIRED')),
    CONSTRAINT ck_devplat_notification_publications_outcome CHECK (
        outcome IS NULL OR outcome IN ('ACCEPTED', 'DUPLICATE', 'HELD', 'REJECTED')),
    CONSTRAINT ck_devplat_notification_publications_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_notification_publications_attempts CHECK (attempts >= 0),
    CONSTRAINT ck_devplat_notification_publications_done CHECK ((state = 'PENDING') = (completed_at IS NULL)),
    -- The envelope is ids only: the catalogue's data keys of the three types, never a
    -- sealed part, never content, an address or a credential secret.
    CONSTRAINT ck_devplat_notification_publications_envelope CHECK (envelope IS NULL OR (
        jsonb_typeof(envelope) = 'object'
        AND octet_length(envelope::text) <= 8192
        AND NOT (envelope ? 'sealed')
        AND jsonb_typeof(envelope -> 'data') = 'object'
        AND (envelope -> 'data') - ARRAY['applicationId', 'credentialId', 'expiresAt', 'decision', 'requestId'] = '{}'::jsonb
        AND envelope ->> 'eventId' = event_id
        AND envelope ->> 'eventType' = event_type))
);
CREATE INDEX ix_devplat_notification_publications_due ON devplat_notification_publications (next_attempt_at)
    WHERE state = 'PENDING';
CREATE INDEX ix_devplat_notification_publications_settled ON devplat_notification_publications (completed_at)
    WHERE state <> 'PENDING';
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_notification_publications;
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
