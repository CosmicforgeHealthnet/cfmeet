//! Section 53 — the central customer webhook platform.
//!
//! * `devplat_webhook_subscriptions` — application-environment-scoped customer endpoints.
//!   The scope (tenant, application, environment, environment kind) is the Platform Access
//!   chain CONFIRMED for the caller, never a client-supplied id. The signing secret is
//!   stored ONLY sealed (`cf_webhooks::secret_box`, AES-256-GCM under a deployment key,
//!   `v2.<kid>.<…>`, bound to the subscription id) and never returned after creation /
//!   rotation. Endpoint health (consecutive failures, last success / failure) drives the
//!   automatic disablement (`disabled_reason`). `origin` tells native subscriptions from
//!   those migrated from Calling (whose ids are preserved). Soft delete only.
//! * `devplat_webhook_events` — the PUBLIC, normalized event (never an internal envelope
//!   or a provider payload verbatim), one row per `(producer, source_event_id)` (dedupe;
//!   the `whe_` event id is stable across retries and replays). Append-only.
//! * `devplat_webhook_deliveries` — one automatic delivery per (subscription, event); a
//!   manual replay is a NEW delivery referencing the original (`replay_of`). Claimed with
//!   a lease (`FOR UPDATE SKIP LOCKED`); bounded retries; FAILED is the dead letter;
//!   SHADOW records a delivery the platform deliberately did not send (shadow mode before
//!   the cut-over from Calling). Never deleted.
//! * `devplat_webhook_attempts` — append-only attempt history: status class and bounded
//!   error class only, never a response body.
//!
//! Rollback drops only these tables (and their guard functions).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE devplat_webhook_subscriptions (
    id                   UUID         NOT NULL,
    tenant_id            UUID         NOT NULL,
    application_id       UUID         NOT NULL,
    environment_id       UUID         NOT NULL,
    environment_kind     VARCHAR(16)  NOT NULL,
    url                  TEXT         NOT NULL,
    description          VARCHAR(200),
    event_types          TEXT[]       NOT NULL,
    status               VARCHAR(16)  NOT NULL,
    secret_sealed        TEXT         NOT NULL,
    secret_key_id        VARCHAR(32)  NOT NULL,
    secret_version       INTEGER      NOT NULL DEFAULT 1,
    version              BIGINT       NOT NULL DEFAULT 1,
    consecutive_failures INTEGER      NOT NULL DEFAULT 0,
    last_success_at      TIMESTAMPTZ,
    last_failure_at      TIMESTAMPTZ,
    disabled_reason      VARCHAR(64),
    origin               VARCHAR(32)  NOT NULL DEFAULT 'NATIVE',
    created_by           VARCHAR(100) NOT NULL,
    created_at           TIMESTAMPTZ  NOT NULL,
    updated_at           TIMESTAMPTZ  NOT NULL,
    deleted_at           TIMESTAMPTZ,
    CONSTRAINT pk_devplat_webhook_subscriptions PRIMARY KEY (id),
    CONSTRAINT uq_devplat_webhook_subscriptions_scope UNIQUE (tenant_id, application_id, environment_id, id),
    CONSTRAINT ck_devplat_webhook_subscriptions_kind CHECK (environment_kind IN ('SANDBOX', 'TEST', 'PRODUCTION')),
    CONSTRAINT ck_devplat_webhook_subscriptions_status CHECK (status IN ('ACTIVE', 'DISABLED', 'DELETED')),
    CONSTRAINT ck_devplat_webhook_subscriptions_deleted CHECK ((status = 'DELETED') = (deleted_at IS NOT NULL)),
    CONSTRAINT ck_devplat_webhook_subscriptions_url CHECK (
        octet_length(url) <= 2048 AND url ~ '^https?://[^@/\s]+(/[^\s]*)?$'),
    CONSTRAINT ck_devplat_webhook_subscriptions_types CHECK (
        cardinality(event_types) BETWEEN 1 AND 64 AND array_position(event_types, NULL) IS NULL),
    -- Only a sealed secret, under the key id it names: never a raw `whsec_` value.
    CONSTRAINT ck_devplat_webhook_subscriptions_sealed CHECK (
        secret_key_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$'
        AND secret_sealed ~ '^v2\.[A-Za-z0-9][A-Za-z0-9_-]{0,31}\.[A-Za-z0-9_-]{40,}$'
        AND left(secret_sealed, length(secret_key_id) + 4) = 'v2.' || secret_key_id || '.'
        AND position('whsec_' IN secret_sealed) = 0),
    CONSTRAINT ck_devplat_webhook_subscriptions_versions CHECK (version >= 1 AND secret_version >= 1),
    CONSTRAINT ck_devplat_webhook_subscriptions_health CHECK (consecutive_failures >= 0),
    CONSTRAINT ck_devplat_webhook_subscriptions_disabled CHECK (
        disabled_reason IS NULL OR (status <> 'ACTIVE' AND disabled_reason ~ '^[A-Z][A-Z0-9_]{0,63}$')),
    CONSTRAINT ck_devplat_webhook_subscriptions_origin CHECK (origin IN ('NATIVE', 'MIGRATED_FROM_CALLING'))
);
CREATE INDEX ix_devplat_webhook_subscriptions_scope
    ON devplat_webhook_subscriptions (environment_id, status, created_at, id);

CREATE FUNCTION devplat_webhook_subscriptions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.environment_kind <> OLD.environment_kind
       OR NEW.origin <> OLD.origin OR NEW.created_by <> OLD.created_by OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'devplat_webhook_subscriptions: scope and identity are immutable' USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.status = 'DELETED' THEN
        RAISE EXCEPTION 'devplat_webhook_subscriptions: DELETED is terminal' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.version < OLD.version OR NEW.secret_version < OLD.secret_version THEN
        RAISE EXCEPTION 'devplat_webhook_subscriptions: versions never decrease' USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_webhook_subscriptions_guard BEFORE UPDATE ON devplat_webhook_subscriptions
    FOR EACH ROW EXECUTE FUNCTION devplat_webhook_subscriptions_guard();
CREATE TRIGGER tr_devplat_webhook_subscriptions_no_delete BEFORE DELETE ON devplat_webhook_subscriptions
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

CREATE TABLE devplat_webhook_events (
    event_id         VARCHAR(36)  NOT NULL,
    producer         VARCHAR(64)  NOT NULL,
    source_event_id  VARCHAR(64)  NOT NULL,
    event_type       VARCHAR(64)  NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    occurred_at      TIMESTAMPTZ  NOT NULL,
    payload          JSONB        NOT NULL,
    received_at      TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_webhook_events PRIMARY KEY (event_id),
    CONSTRAINT uq_devplat_webhook_events_source UNIQUE (producer, source_event_id),
    CONSTRAINT uq_devplat_webhook_events_scope UNIQUE (tenant_id, application_id, environment_id, event_id),
    CONSTRAINT ck_devplat_webhook_events_id CHECK (event_id ~ '^whe_[0-9a-f]{32}$'),
    CONSTRAINT ck_devplat_webhook_events_producer CHECK (producer IN (
        'cfmeet.meeting-service', 'cfmeet.calling', 'cfmeet.privacy-relay', 'cfmeet.messaging',
        'cfmeet.numbering', 'cfmeet.sms-gateway', 'cfmeet.developer-platform')),
    CONSTRAINT ck_devplat_webhook_events_source CHECK (source_event_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    CONSTRAINT ck_devplat_webhook_events_type CHECK (event_type ~ '^[a-z][a-z_]*(\.[a-z_]+)+$'),
    CONSTRAINT ck_devplat_webhook_events_payload CHECK (
        jsonb_typeof(payload) = 'object' AND payload ->> 'id' = event_id
        AND payload ->> 'type' = event_type AND jsonb_typeof(payload -> 'data') = 'object'
        AND octet_length(payload::text) <= 16384)
);
CREATE INDEX ix_devplat_webhook_events_scope ON devplat_webhook_events (environment_id, occurred_at DESC);
CREATE TRIGGER tr_devplat_webhook_events_append_only BEFORE UPDATE OR DELETE ON devplat_webhook_events
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_webhook_events_no_truncate BEFORE TRUNCATE ON devplat_webhook_events
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();

CREATE TABLE devplat_webhook_deliveries (
    id               UUID         NOT NULL,
    subscription_id  UUID         NOT NULL,
    event_id         VARCHAR(36)  NOT NULL,
    tenant_id        UUID         NOT NULL,
    application_id   UUID         NOT NULL,
    environment_id   UUID         NOT NULL,
    replay_of        UUID,
    status           VARCHAR(16)  NOT NULL,
    attempt_count    INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL,
    lease_until      TIMESTAMPTZ,
    last_http_status INTEGER,
    failure_category VARCHAR(64),
    created_at       TIMESTAMPTZ  NOT NULL,
    updated_at       TIMESTAMPTZ  NOT NULL,
    completed_at     TIMESTAMPTZ,
    CONSTRAINT pk_devplat_webhook_deliveries PRIMARY KEY (id),
    -- The subscription and the event belong to the SAME application environment.
    CONSTRAINT fk_devplat_webhook_deliveries_subscription
        FOREIGN KEY (tenant_id, application_id, environment_id, subscription_id)
        REFERENCES devplat_webhook_subscriptions (tenant_id, application_id, environment_id, id),
    CONSTRAINT fk_devplat_webhook_deliveries_event
        FOREIGN KEY (tenant_id, application_id, environment_id, event_id)
        REFERENCES devplat_webhook_events (tenant_id, application_id, environment_id, event_id),
    CONSTRAINT fk_devplat_webhook_deliveries_replay FOREIGN KEY (replay_of) REFERENCES devplat_webhook_deliveries (id),
    CONSTRAINT ck_devplat_webhook_deliveries_status CHECK (status IN
        ('PENDING', 'DELIVERING', 'RETRY_SCHEDULED', 'SUCCEEDED', 'FAILED', 'CANCELLED', 'SHADOW')),
    CONSTRAINT ck_devplat_webhook_deliveries_completed CHECK (
        (status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'SHADOW')) = (completed_at IS NOT NULL)),
    CONSTRAINT ck_devplat_webhook_deliveries_lease CHECK ((status = 'DELIVERING') = (lease_until IS NOT NULL)),
    CONSTRAINT ck_devplat_webhook_deliveries_attempts CHECK (attempt_count BETWEEN 0 AND 64),
    CONSTRAINT ck_devplat_webhook_deliveries_http CHECK (last_http_status IS NULL OR last_http_status BETWEEN 100 AND 599),
    CONSTRAINT ck_devplat_webhook_deliveries_category CHECK (
        failure_category IS NULL OR failure_category ~ '^[A-Z][A-Z0-9_]{1,63}$')
);
-- One automatic delivery per (subscription, event): a re-published event never fans out
-- twice. Replays are separate rows referencing the original.
CREATE UNIQUE INDEX uq_devplat_webhook_deliveries_original
    ON devplat_webhook_deliveries (subscription_id, event_id) WHERE replay_of IS NULL;
CREATE INDEX ix_devplat_webhook_deliveries_due
    ON devplat_webhook_deliveries (next_attempt_at) WHERE status IN ('PENDING', 'RETRY_SCHEDULED', 'DELIVERING');
CREATE INDEX ix_devplat_webhook_deliveries_subscription
    ON devplat_webhook_deliveries (subscription_id, created_at, id);

CREATE FUNCTION devplat_webhook_deliveries_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.subscription_id <> OLD.subscription_id OR NEW.event_id <> OLD.event_id
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.replay_of IS DISTINCT FROM OLD.replay_of
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'SHADOW') THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: % is terminal', OLD.status USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.attempt_count < OLD.attempt_count THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: attempts never decrease' USING ERRCODE = 'check_violation';
    END IF;
    IF NOT (OLD.status || '>' || NEW.status) = ANY (ARRAY[
        'PENDING>DELIVERING', 'RETRY_SCHEDULED>DELIVERING', 'DELIVERING>DELIVERING',
        'DELIVERING>SUCCEEDED', 'DELIVERING>RETRY_SCHEDULED', 'DELIVERING>FAILED',
        'PENDING>CANCELLED', 'RETRY_SCHEDULED>CANCELLED', 'DELIVERING>CANCELLED',
        'PENDING>SHADOW', 'RETRY_SCHEDULED>SHADOW', 'DELIVERING>SHADOW']) THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: % -> % is not a delivery transition', OLD.status, NEW.status
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER tr_devplat_webhook_deliveries_guard BEFORE UPDATE ON devplat_webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION devplat_webhook_deliveries_guard();
CREATE TRIGGER tr_devplat_webhook_deliveries_no_delete BEFORE DELETE ON devplat_webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION devplat_no_delete();

CREATE TABLE devplat_webhook_attempts (
    delivery_id      UUID         NOT NULL,
    attempt_number   INTEGER      NOT NULL,
    started_at       TIMESTAMPTZ  NOT NULL,
    finished_at      TIMESTAMPTZ  NOT NULL,
    http_status      INTEGER,
    outcome          VARCHAR(16)  NOT NULL,
    error_class      VARCHAR(64),
    CONSTRAINT pk_devplat_webhook_attempts PRIMARY KEY (delivery_id, attempt_number),
    CONSTRAINT fk_devplat_webhook_attempts_delivery FOREIGN KEY (delivery_id) REFERENCES devplat_webhook_deliveries (id),
    CONSTRAINT ck_devplat_webhook_attempts_number CHECK (attempt_number BETWEEN 1 AND 64),
    CONSTRAINT ck_devplat_webhook_attempts_outcome CHECK (outcome IN ('SUCCEEDED', 'RETRY', 'FAILED')),
    CONSTRAINT ck_devplat_webhook_attempts_http CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599),
    CONSTRAINT ck_devplat_webhook_attempts_class CHECK (error_class IS NULL OR error_class ~ '^[A-Z][A-Z0-9_]{1,63}$'),
    CONSTRAINT ck_devplat_webhook_attempts_time CHECK (finished_at >= started_at)
);
CREATE TRIGGER tr_devplat_webhook_attempts_append_only BEFORE UPDATE OR DELETE ON devplat_webhook_attempts
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_webhook_attempts_no_truncate BEFORE TRUNCATE ON devplat_webhook_attempts
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS devplat_webhook_attempts;
DROP TABLE IF EXISTS devplat_webhook_deliveries;
DROP TABLE IF EXISTS devplat_webhook_events;
DROP TABLE IF EXISTS devplat_webhook_subscriptions;
DROP FUNCTION IF EXISTS devplat_webhook_deliveries_guard();
DROP FUNCTION IF EXISTS devplat_webhook_subscriptions_guard();
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
