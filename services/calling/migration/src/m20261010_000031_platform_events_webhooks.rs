//! Part G §67 / §68: durable event publication and customer webhooks (ADR 0028).
//!
//! * `calling_outbox_events` gains the processing state of a multi-worker publisher:
//!   a claim lease (`lease_until`, `FOR UPDATE SKIP LOCKED` + lease = crash recovery), the
//!   last bounded failure code, and `DEAD_LETTER` after bounded retries. Allowed
//!   transitions: PENDING → PUBLISHED, PENDING → DEAD_LETTER, and DEAD_LETTER → PENDING
//!   (operator replay). Envelopes stay immutable; nothing pending is ever deleted.
//! * `webhook_subscriptions` — application-scoped customer endpoints; the signing secret
//!   is stored sealed (AES-256-GCM under a deployment key) and never returned after
//!   creation / rotation.
//! * `webhook_events` — the PUBLIC, normalized event (never a provider payload or an
//!   internal envelope verbatim), one row per source event id (dedupe).
//! * `webhook_deliveries` — one delivery per (subscription, event); a manual replay is a
//!   NEW delivery referencing the original. Claimed with a lease; bounded retries; FAILED
//!   is the dead letter.
//! * `webhook_delivery_attempts` — append-only attempt history (status class / category,
//!   never a response body).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE calling_outbox_events
    ADD COLUMN lease_until     TIMESTAMPTZ,
    ADD COLUMN last_error_code VARCHAR(64),
    DROP CONSTRAINT ck_calling_outbox_events_status,
    ADD CONSTRAINT ck_calling_outbox_events_status
        CHECK (status IN ('PENDING', 'PUBLISHED', 'DEAD_LETTER')),
    ADD CONSTRAINT ck_calling_outbox_events_error_code
        CHECK (last_error_code IS NULL OR last_error_code ~ '^[A-Z][A-Z0-9_]{1,63}$');
DROP TRIGGER trg_calling_outbox_events_status_transition ON calling_outbox_events;
CREATE TRIGGER trg_calling_outbox_events_status_transition
    BEFORE UPDATE ON calling_outbox_events
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'PENDING>PUBLISHED', 'PENDING>DEAD_LETTER', 'DEAD_LETTER>PENDING');
CREATE INDEX idx_calling_outbox_events_dead_letter
    ON calling_outbox_events (updated_at) WHERE status = 'DEAD_LETTER';

CREATE TABLE webhook_subscriptions (
    id                        UUID PRIMARY KEY,
    tenant_id                 UUID         NOT NULL,
    application_id            UUID         NOT NULL,
    environment_id            UUID         NOT NULL,
    endpoint_url              TEXT         NOT NULL,
    description               VARCHAR(200),
    status                    TEXT         NOT NULL,
    event_types               TEXT[]       NOT NULL,
    secret_sealed             TEXT         NOT NULL,
    secret_version            INTEGER      NOT NULL,
    created_by_credential_id  UUID,
    version                   BIGINT       NOT NULL,
    created_at                TIMESTAMPTZ  NOT NULL,
    updated_at                TIMESTAMPTZ  NOT NULL,
    deleted_at                TIMESTAMPTZ,
    CONSTRAINT uq_webhook_subscriptions_scope UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_webhook_subscriptions_status CHECK (status IN ('ACTIVE', 'DISABLED', 'DELETED')),
    CONSTRAINT ck_webhook_subscriptions_deleted CHECK ((status = 'DELETED') = (deleted_at IS NOT NULL)),
    CONSTRAINT ck_webhook_subscriptions_url CHECK (
        octet_length(endpoint_url) <= 2048 AND endpoint_url ~ '^https?://[^@/\s]+(/[^\s]*)?$'),
    CONSTRAINT ck_webhook_subscriptions_types CHECK (
        cardinality(event_types) BETWEEN 1 AND 64
        AND array_position(event_types, NULL) IS NULL),
    CONSTRAINT ck_webhook_subscriptions_secret CHECK (secret_sealed ~ '^v1\.[A-Za-z0-9_-]+$'),
    CONSTRAINT ck_webhook_subscriptions_versions CHECK (version >= 1 AND secret_version >= 1)
);
CREATE INDEX idx_webhook_subscriptions_scope
    ON webhook_subscriptions (tenant_id, application_id, environment_id, status, created_at);
CREATE TRIGGER trg_webhook_subscriptions_immutable
    BEFORE UPDATE ON webhook_subscriptions
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'created_by_credential_id', 'created_at');
CREATE TRIGGER trg_webhook_subscriptions_status
    BEFORE UPDATE ON webhook_subscriptions
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'ACTIVE>DISABLED', 'DISABLED>ACTIVE', 'ACTIVE>DELETED', 'DISABLED>DELETED');

CREATE TABLE webhook_events (
    id              UUID PRIMARY KEY,
    event_id        VARCHAR(36)  NOT NULL,
    source_event_id VARCHAR(64)  NOT NULL,
    producer        TEXT         NOT NULL,
    event_type      VARCHAR(64)  NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    environment_id  UUID         NOT NULL,
    occurred_at     TIMESTAMPTZ  NOT NULL,
    body            JSONB        NOT NULL,
    created_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT uq_webhook_events_event_id UNIQUE (event_id),
    CONSTRAINT uq_webhook_events_source UNIQUE (producer, source_event_id),
    CONSTRAINT uq_webhook_events_scope UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_webhook_events_event_id CHECK (event_id ~ '^whe_[0-9a-f]{32}$'),
    CONSTRAINT ck_webhook_events_producer CHECK (producer IN ('cfmeet.calling', 'cfmeet.privacy-relay')),
    CONSTRAINT ck_webhook_events_type CHECK (event_type ~ '^[a-z]+(\.[a-z_]+)+$'),
    CONSTRAINT ck_webhook_events_body CHECK (
        jsonb_typeof(body) = 'object' AND body ->> 'id' = event_id
        AND body ->> 'type' = event_type AND octet_length(body::text) <= 16384)
);
CREATE INDEX idx_webhook_events_scope
    ON webhook_events (tenant_id, application_id, environment_id, occurred_at DESC);
CREATE TRIGGER trg_webhook_events_immutable
    BEFORE UPDATE OR DELETE ON webhook_events
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

CREATE TABLE webhook_deliveries (
    id               UUID PRIMARY KEY,
    webhook_event_id UUID         NOT NULL,
    subscription_id  UUID         NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    replay_of        UUID,
    status           TEXT         NOT NULL,
    attempt_count    INTEGER      NOT NULL,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_until      TIMESTAMPTZ,
    last_http_status INTEGER,
    failure_category VARCHAR(64),
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT fk_webhook_deliveries_event
        FOREIGN KEY (tenant_id, application_id, environment_id, webhook_event_id)
        REFERENCES webhook_events (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_webhook_deliveries_subscription
        FOREIGN KEY (tenant_id, application_id, environment_id, subscription_id)
        REFERENCES webhook_subscriptions (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_webhook_deliveries_replay
        FOREIGN KEY (replay_of) REFERENCES webhook_deliveries (id) ON DELETE RESTRICT,
    CONSTRAINT ck_webhook_deliveries_status CHECK (status IN
        ('PENDING', 'DELIVERING', 'RETRY_SCHEDULED', 'SUCCEEDED', 'FAILED', 'CANCELLED')),
    CONSTRAINT ck_webhook_deliveries_completed CHECK (
        (status IN ('SUCCEEDED', 'FAILED', 'CANCELLED')) = (completed_at IS NOT NULL)),
    CONSTRAINT ck_webhook_deliveries_attempts CHECK (attempt_count >= 0 AND attempt_count <= 64),
    CONSTRAINT ck_webhook_deliveries_http CHECK (last_http_status IS NULL OR last_http_status BETWEEN 100 AND 599),
    CONSTRAINT ck_webhook_deliveries_category CHECK (
        failure_category IS NULL OR failure_category ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
-- One automatic delivery per (subscription, event): a re-published event never fans out
-- twice. Replays are separate rows referencing the original.
CREATE UNIQUE INDEX uq_webhook_deliveries_original
    ON webhook_deliveries (subscription_id, webhook_event_id) WHERE replay_of IS NULL;
CREATE INDEX idx_webhook_deliveries_due
    ON webhook_deliveries (next_attempt_at) WHERE status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING');
CREATE INDEX idx_webhook_deliveries_scope
    ON webhook_deliveries (tenant_id, application_id, environment_id, created_at DESC);
CREATE TRIGGER trg_webhook_deliveries_immutable
    BEFORE UPDATE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'webhook_event_id', 'subscription_id', 'tenant_id', 'application_id',
        'environment_id', 'replay_of', 'created_at');
CREATE TRIGGER trg_webhook_deliveries_status
    BEFORE UPDATE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status',
        'PENDING>DELIVERING', 'RETRY_SCHEDULED>DELIVERING', 'DELIVERING>DELIVERING',
        'DELIVERING>SUCCEEDED', 'DELIVERING>RETRY_SCHEDULED', 'DELIVERING>FAILED',
        'PENDING>CANCELLED', 'RETRY_SCHEDULED>CANCELLED', 'DELIVERING>CANCELLED');
CREATE TRIGGER trg_webhook_deliveries_no_delete
    BEFORE DELETE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

CREATE TABLE webhook_delivery_attempts (
    delivery_id      UUID         NOT NULL REFERENCES webhook_deliveries (id) ON DELETE RESTRICT,
    attempt_number   INTEGER      NOT NULL,
    outcome          TEXT         NOT NULL,
    http_status      INTEGER,
    failure_category VARCHAR(64),
    started_at       TIMESTAMPTZ  NOT NULL,
    finished_at      TIMESTAMPTZ  NOT NULL,
    PRIMARY KEY (delivery_id, attempt_number),
    CONSTRAINT ck_webhook_delivery_attempts_outcome CHECK (outcome IN ('SUCCEEDED', 'RETRY', 'FAILED')),
    CONSTRAINT ck_webhook_delivery_attempts_http CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599)
);
CREATE TRIGGER trg_webhook_delivery_attempts_immutable
    BEFORE UPDATE OR DELETE ON webhook_delivery_attempts
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS webhook_delivery_attempts;
DROP TABLE IF EXISTS webhook_deliveries;
DROP TABLE IF EXISTS webhook_events;
DROP TABLE IF EXISTS webhook_subscriptions;
DROP INDEX IF EXISTS idx_calling_outbox_events_dead_letter;
DROP TRIGGER trg_calling_outbox_events_status_transition ON calling_outbox_events;
UPDATE calling_outbox_events SET status = 'PENDING' WHERE status = 'DEAD_LETTER';
CREATE TRIGGER trg_calling_outbox_events_status_transition
    BEFORE UPDATE ON calling_outbox_events
    FOR EACH ROW EXECUTE FUNCTION calling_guard_state_transition('status', 'PENDING>PUBLISHED');
ALTER TABLE calling_outbox_events
    DROP CONSTRAINT ck_calling_outbox_events_error_code,
    DROP CONSTRAINT ck_calling_outbox_events_status,
    ADD CONSTRAINT ck_calling_outbox_events_status CHECK (status IN ('PENDING', 'PUBLISHED')),
    DROP COLUMN last_error_code,
    DROP COLUMN lease_until;
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
