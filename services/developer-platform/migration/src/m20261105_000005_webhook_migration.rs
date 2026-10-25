//! Section 53 §9.1 / §19.3 — the controlled migration of customer webhooks from Calling.
//!
//! * `devplat_webhook_subscriptions.secret_state` — `VALID`, or `ROTATION_REQUIRED` for a
//!   migrated subscription whose original signing secret could not be carried over under
//!   controlled custody (Calling could not open it, or its transfer seal did not open
//!   here). Such a row holds NO secret at all (`secret_sealed` / `secret_key_id` NULL), is
//!   never ACTIVE, and becomes VALID only through a customer rotation — a substitute secret
//!   is never generated.
//! * `devplat_webhook_events.origin` / `devplat_webhook_deliveries.origin` — `NATIVE` or
//!   `MIGRATED_FROM_CALLING` (immutable imported history; ids preserved). The source
//!   dedupe key `(producer, source_event_id)` is unique PER ORIGIN: an event both services
//!   ingested during the shadow window exists once natively and once as Calling's history.
//! * `devplat_webhook_deliveries.migration_hold` — an imported delivery still open at
//!   Calling: a mirror of Calling's state (re-import updates it), never claimed, shadowed
//!   or sent until the takeover releases it.
//! * `devplat_webhook_migration_runs` — resumable import runs (state, phase, cursor per
//!   entity, counts, bounded conflicts, operator); one open run at a time.
//! * `devplat_webhook_migration_subscriptions` — the Calling version / status / secret
//!   version each migrated subscription was imported at, its secret outcome and the
//!   acknowledgement of a required rotation.
//! * `devplat_webhook_migration_state` — the single cutover state row (`PREPARING`,
//!   `CUT_OVER`, `ROLLED_BACK`): deliveries to migrated subscriptions are sent only once
//!   CUT_OVER, nothing is sent once ROLLED_BACK.
//! * `devplat_webhook_migration_audit` — append-only operator audit of every migration
//!   command (codes and counts only; never a secret, a seal or an endpoint URL).
//!
//! Rollback drops the migration tables and columns, restoring the original constraints and
//! guard (refused while a ROTATION_REQUIRED row exists — it holds no secret).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE devplat_webhook_subscriptions
    ADD COLUMN secret_state VARCHAR(24) NOT NULL DEFAULT 'VALID',
    ALTER COLUMN secret_sealed DROP NOT NULL,
    ALTER COLUMN secret_key_id DROP NOT NULL,
    DROP CONSTRAINT ck_devplat_webhook_subscriptions_sealed,
    ADD CONSTRAINT ck_devplat_webhook_subscriptions_secret_state
        CHECK (secret_state IN ('VALID', 'ROTATION_REQUIRED')),
    -- Only a sealed secret, under the key id it names (never a raw `whsec_` value) — or,
    -- for a migrated subscription whose secret could not be carried over, no secret at all
    -- and never ACTIVE (the customer must rotate).
    ADD CONSTRAINT ck_devplat_webhook_subscriptions_sealed CHECK (
        (secret_state = 'VALID'
            AND secret_sealed IS NOT NULL AND secret_key_id IS NOT NULL
            AND secret_key_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$'
            AND secret_sealed ~ '^v2\.[A-Za-z0-9][A-Za-z0-9_-]{0,31}\.[A-Za-z0-9_-]{40,}$'
            AND left(secret_sealed, length(secret_key_id) + 4) = 'v2.' || secret_key_id || '.'
            AND position('whsec_' IN secret_sealed) = 0)
        OR (secret_state = 'ROTATION_REQUIRED'
            AND secret_sealed IS NULL AND secret_key_id IS NULL
            AND status <> 'ACTIVE' AND origin = 'MIGRATED_FROM_CALLING'));

ALTER TABLE devplat_webhook_events
    ADD COLUMN origin VARCHAR(32) NOT NULL DEFAULT 'NATIVE',
    ADD CONSTRAINT ck_devplat_webhook_events_origin CHECK (origin IN ('NATIVE', 'MIGRATED_FROM_CALLING')),
    DROP CONSTRAINT uq_devplat_webhook_events_source;
CREATE UNIQUE INDEX uq_devplat_webhook_events_source_native
    ON devplat_webhook_events (producer, source_event_id) WHERE origin = 'NATIVE';
CREATE UNIQUE INDEX uq_devplat_webhook_events_source_migrated
    ON devplat_webhook_events (producer, source_event_id) WHERE origin = 'MIGRATED_FROM_CALLING';

ALTER TABLE devplat_webhook_deliveries
    ADD COLUMN origin VARCHAR(32) NOT NULL DEFAULT 'NATIVE',
    ADD COLUMN migration_hold BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT ck_devplat_webhook_deliveries_origin CHECK (origin IN ('NATIVE', 'MIGRATED_FROM_CALLING')),
    ADD CONSTRAINT ck_devplat_webhook_deliveries_hold CHECK (NOT migration_hold OR origin = 'MIGRATED_FROM_CALLING');
CREATE INDEX ix_devplat_webhook_deliveries_held
    ON devplat_webhook_deliveries (subscription_id) WHERE migration_hold;

-- A held (mirrored) delivery follows Calling's state until the takeover releases it;
-- identity stays immutable, a hold is never re-applied, everything else is unchanged.
CREATE OR REPLACE FUNCTION devplat_webhook_deliveries_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id <> OLD.id OR NEW.subscription_id <> OLD.subscription_id OR NEW.event_id <> OLD.event_id
       OR NEW.tenant_id <> OLD.tenant_id OR NEW.application_id <> OLD.application_id
       OR NEW.environment_id <> OLD.environment_id OR NEW.replay_of IS DISTINCT FROM OLD.replay_of
       OR NEW.created_at <> OLD.created_at OR NEW.origin <> OLD.origin THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: identity is immutable' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.migration_hold AND NOT OLD.migration_hold THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: a released delivery is never held again' USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.attempt_count < OLD.attempt_count THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: attempts never decrease' USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.migration_hold THEN
        RETURN NEW;
    END IF;
    IF OLD.status IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'SHADOW') THEN
        RAISE EXCEPTION 'devplat_webhook_deliveries: % is terminal', OLD.status USING ERRCODE = 'check_violation';
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

CREATE TABLE devplat_webhook_migration_runs (
    id            UUID         NOT NULL,
    state         VARCHAR(16)  NOT NULL,
    phase         VARCHAR(16)  NOT NULL,
    cursors       JSONB        NOT NULL DEFAULT '{}'::jsonb,
    counts        JSONB        NOT NULL DEFAULT '{}'::jsonb,
    conflicts     JSONB        NOT NULL DEFAULT '[]'::jsonb,
    pages         INTEGER      NOT NULL DEFAULT 0,
    calling_start JSONB        NOT NULL DEFAULT '{}'::jsonb,
    last_error    VARCHAR(64),
    operator      VARCHAR(100) NOT NULL,
    started_at    TIMESTAMPTZ  NOT NULL,
    updated_at    TIMESTAMPTZ  NOT NULL,
    finished_at   TIMESTAMPTZ,
    CONSTRAINT pk_devplat_webhook_migration_runs PRIMARY KEY (id),
    CONSTRAINT ck_devplat_webhook_migration_runs_state CHECK (state IN ('RUNNING', 'FAILED', 'COMPLETED')),
    CONSTRAINT ck_devplat_webhook_migration_runs_phase CHECK (
        phase IN ('SUBSCRIPTIONS', 'EVENTS', 'DELIVERIES', 'ATTEMPTS', 'DONE')),
    CONSTRAINT ck_devplat_webhook_migration_runs_done CHECK ((state = 'COMPLETED') = (finished_at IS NOT NULL)),
    CONSTRAINT ck_devplat_webhook_migration_runs_completed CHECK (state <> 'COMPLETED' OR phase = 'DONE'),
    CONSTRAINT ck_devplat_webhook_migration_runs_error CHECK (
        last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    CONSTRAINT ck_devplat_webhook_migration_runs_pages CHECK (pages >= 0)
);
-- One open (resumable) run at a time.
CREATE UNIQUE INDEX uq_devplat_webhook_migration_runs_open
    ON devplat_webhook_migration_runs ((true)) WHERE state <> 'COMPLETED';
CREATE INDEX ix_devplat_webhook_migration_runs_started ON devplat_webhook_migration_runs (started_at DESC);

CREATE TABLE devplat_webhook_migration_subscriptions (
    subscription_id          UUID         NOT NULL,
    calling_version          BIGINT       NOT NULL,
    calling_status           VARCHAR(16)  NOT NULL,
    calling_secret_version   INTEGER      NOT NULL,
    imported_version         BIGINT       NOT NULL,
    secret_outcome           VARCHAR(40)  NOT NULL,
    run_id                   UUID,
    imported_at              TIMESTAMPTZ  NOT NULL,
    updated_at               TIMESTAMPTZ  NOT NULL,
    rotation_acknowledged_by VARCHAR(100),
    rotation_acknowledged_at TIMESTAMPTZ,
    CONSTRAINT pk_devplat_webhook_migration_subscriptions PRIMARY KEY (subscription_id),
    CONSTRAINT fk_devplat_webhook_migration_subscriptions_subscription
        FOREIGN KEY (subscription_id) REFERENCES devplat_webhook_subscriptions (id),
    CONSTRAINT ck_devplat_webhook_migration_subscriptions_status CHECK (
        calling_status IN ('ACTIVE', 'DISABLED', 'DELETED')),
    CONSTRAINT ck_devplat_webhook_migration_subscriptions_outcome CHECK (secret_outcome IN (
        'TRANSFERRED', 'ROTATION_REQUIRED_UNOPENABLE', 'ROTATION_REQUIRED_TRANSFER', 'PUSHED_TO_CALLING')),
    CONSTRAINT ck_devplat_webhook_migration_subscriptions_ack CHECK (
        (rotation_acknowledged_by IS NULL) = (rotation_acknowledged_at IS NULL))
);

CREATE TABLE devplat_webhook_migration_state (
    id             SMALLINT     NOT NULL,
    phase          VARCHAR(16)  NOT NULL,
    changed_by     VARCHAR(100) NOT NULL,
    changed_at     TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_devplat_webhook_migration_state PRIMARY KEY (id),
    CONSTRAINT ck_devplat_webhook_migration_state_single CHECK (id = 1),
    CONSTRAINT ck_devplat_webhook_migration_state_phase CHECK (phase IN ('PREPARING', 'CUT_OVER', 'ROLLED_BACK'))
);

CREATE TABLE devplat_webhook_migration_audit (
    id          UUID         NOT NULL,
    operator    VARCHAR(100) NOT NULL,
    action      VARCHAR(64)  NOT NULL,
    run_id      UUID,
    summary     JSONB        NOT NULL DEFAULT '{}'::jsonb,
    request_id  VARCHAR(64)  NOT NULL,
    occurred_at TIMESTAMPTZ  NOT NULL,
    seq         BIGSERIAL    NOT NULL,
    CONSTRAINT pk_devplat_webhook_migration_audit PRIMARY KEY (id),
    CONSTRAINT uq_devplat_webhook_migration_audit_seq UNIQUE (seq),
    CONSTRAINT ck_devplat_webhook_migration_audit_action CHECK (action ~ '^webhook_migration\.[a-z_]+$'),
    CONSTRAINT ck_devplat_webhook_migration_audit_summary CHECK (
        jsonb_typeof(summary) = 'object' AND position('whsec_' IN summary::text) = 0
        AND octet_length(summary::text) <= 16384)
);
CREATE TRIGGER tr_devplat_webhook_migration_audit_append_only BEFORE UPDATE OR DELETE ON devplat_webhook_migration_audit
    FOR EACH ROW EXECUTE FUNCTION devplat_append_only();
CREATE TRIGGER tr_devplat_webhook_migration_audit_no_truncate BEFORE TRUNCATE ON devplat_webhook_migration_audit
    FOR EACH STATEMENT EXECUTE FUNCTION devplat_append_only();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM devplat_webhook_subscriptions WHERE secret_state = 'ROTATION_REQUIRED') THEN
        RAISE EXCEPTION 'subscriptions awaiting a secret rotation exist; they hold no secret and cannot be rolled back';
    END IF;
END $$;
DROP TABLE IF EXISTS devplat_webhook_migration_audit;
DROP TABLE IF EXISTS devplat_webhook_migration_state;
DROP TABLE IF EXISTS devplat_webhook_migration_subscriptions;
DROP TABLE IF EXISTS devplat_webhook_migration_runs;
CREATE OR REPLACE FUNCTION devplat_webhook_deliveries_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
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
DROP INDEX IF EXISTS ix_devplat_webhook_deliveries_held;
ALTER TABLE devplat_webhook_deliveries
    DROP CONSTRAINT IF EXISTS ck_devplat_webhook_deliveries_hold,
    DROP CONSTRAINT IF EXISTS ck_devplat_webhook_deliveries_origin,
    DROP COLUMN IF EXISTS migration_hold,
    DROP COLUMN IF EXISTS origin;
DROP INDEX IF EXISTS uq_devplat_webhook_events_source_migrated;
DROP INDEX IF EXISTS uq_devplat_webhook_events_source_native;
ALTER TABLE devplat_webhook_events
    DROP CONSTRAINT IF EXISTS ck_devplat_webhook_events_origin,
    DROP COLUMN IF EXISTS origin,
    ADD CONSTRAINT uq_devplat_webhook_events_source UNIQUE (producer, source_event_id);
ALTER TABLE devplat_webhook_subscriptions
    DROP CONSTRAINT ck_devplat_webhook_subscriptions_sealed,
    DROP CONSTRAINT IF EXISTS ck_devplat_webhook_subscriptions_secret_state,
    DROP COLUMN IF EXISTS secret_state,
    ALTER COLUMN secret_sealed SET NOT NULL,
    ALTER COLUMN secret_key_id SET NOT NULL,
    ADD CONSTRAINT ck_devplat_webhook_subscriptions_sealed CHECK (
        secret_key_id ~ '^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$'
        AND secret_sealed ~ '^v2\.[A-Za-z0-9][A-Za-z0-9_-]{0,31}\.[A-Za-z0-9_-]{40,}$'
        AND left(secret_sealed, length(secret_key_id) + 4) = 'v2.' || secret_key_id || '.'
        AND position('whsec_' IN secret_sealed) = 0);
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
