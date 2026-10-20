//! Part V J §§130–145 — operations: kill switches, incidents and the operator audit.
//!
//! * `messaging_kill_switches` — operator switches that stop one capability globally
//!   (`scope_key = 'GLOBAL'`) or for one application environment (its UUID). Each guarded
//!   operation fails closed with FEATURE_DISABLED / KILL_SWITCH_ENGAGED while engaged;
//!   workers pause (their queues are durable, nothing is lost). Switching never deletes
//!   data and never weakens a security property (there is no switch that enables
//!   anything).
//! * `messaging_incidents` — conditions Messaging refuses to guess about (dead-lettered
//!   relays, a key-transparency rollback). De-duplicated while OPEN; summaries are fixed
//!   safe sentences.
//! * `messaging_operator_audit` — every operator change (switches, incident resolution),
//!   append-only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_kill_switches (
    switch_name   VARCHAR(32)  NOT NULL,
    scope_key     VARCHAR(36)  NOT NULL,
    engaged       BOOLEAN      NOT NULL,
    reason        VARCHAR(48)  NOT NULL,
    changed_by    VARCHAR(64)  NOT NULL,
    changed_at    TIMESTAMPTZ  NOT NULL,
    version       INTEGER      NOT NULL DEFAULT 1,
    CONSTRAINT pk_messaging_kill_switches PRIMARY KEY (switch_name, scope_key),
    CONSTRAINT ck_messaging_kill_switches_name CHECK (switch_name IN (
        'MESSAGE_SEND', 'CONVERSATION_CREATE', 'E2EE_ONBOARDING', 'ATTACHMENT_UPLOAD',
        'SMS_BRIDGE', 'WEBHOOK_RELAY')),
    CONSTRAINT ck_messaging_kill_switches_scope CHECK (
        scope_key = 'GLOBAL' OR scope_key ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'),
    CONSTRAINT ck_messaging_kill_switches_reason CHECK (reason ~ '^[A-Z][A-Z0-9_]{1,47}$'),
    CONSTRAINT ck_messaging_kill_switches_operator CHECK (changed_by ~ '^[A-Za-z0-9._@-]{1,64}$')
);

CREATE TABLE messaging_incidents (
    id              UUID         NOT NULL,
    category        VARCHAR(48)  NOT NULL,
    severity        VARCHAR(16)  NOT NULL,
    dedupe_key      VARCHAR(128) NOT NULL,
    status          VARCHAR(16)  NOT NULL DEFAULT 'OPEN',
    summary         VARCHAR(256) NOT NULL,
    occurrences     INTEGER      NOT NULL DEFAULT 1,
    first_seen_at   TIMESTAMPTZ  NOT NULL,
    last_seen_at    TIMESTAMPTZ  NOT NULL,
    resolved_at     TIMESTAMPTZ,
    resolved_by     VARCHAR(64),
    CONSTRAINT pk_messaging_incidents PRIMARY KEY (id),
    CONSTRAINT ck_messaging_incidents_category CHECK (category IN (
        'OUTBOX_DEAD_LETTER', 'WEBHOOK_RELAY_DEAD_LETTER', 'SMS_TRANSPORT_FAILED',
        'ATTACHMENT_DELETE_DEAD_LETTER', 'KT_ROLLBACK_DETECTED')),
    CONSTRAINT ck_messaging_incidents_severity CHECK (severity IN ('LOW', 'MEDIUM', 'HIGH', 'CRITICAL')),
    CONSTRAINT ck_messaging_incidents_status CHECK (status IN ('OPEN', 'RESOLVED')),
    CONSTRAINT ck_messaging_incidents_resolution CHECK ((status = 'RESOLVED') = (resolved_at IS NOT NULL AND resolved_by IS NOT NULL))
);
CREATE UNIQUE INDEX uq_messaging_incidents_open ON messaging_incidents (dedupe_key) WHERE status = 'OPEN';

CREATE TABLE messaging_operator_audit (
    id           UUID         NOT NULL,
    occurred_at  TIMESTAMPTZ  NOT NULL,
    operator_id  VARCHAR(64)  NOT NULL,
    action       VARCHAR(48)  NOT NULL,
    target       VARCHAR(128) NOT NULL,
    detail       JSONB        NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT pk_messaging_operator_audit PRIMARY KEY (id),
    CONSTRAINT ck_messaging_operator_audit_action CHECK (action ~ '^[A-Z][A-Z0-9_]{1,47}$')
);
CREATE FUNCTION messaging_operator_audit_immutable() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'messaging_operator_audit is append-only';
END $$;
CREATE TRIGGER trg_messaging_operator_audit_immutable BEFORE UPDATE OR DELETE ON messaging_operator_audit
    FOR EACH ROW EXECUTE FUNCTION messaging_operator_audit_immutable();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_messaging_operator_audit_immutable ON messaging_operator_audit;
DROP FUNCTION IF EXISTS messaging_operator_audit_immutable();
DROP TABLE IF EXISTS messaging_operator_audit;
DROP TABLE IF EXISTS messaging_incidents;
DROP TABLE IF EXISTS messaging_kill_switches;
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
