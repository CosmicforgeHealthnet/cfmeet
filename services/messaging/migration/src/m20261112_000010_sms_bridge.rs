//! Part V H §§106–112 — the SMS Bridge, conversation side.
//!
//! * `messaging_messages.channel` — `IN_APP` (default) or `SMS` (a message that arrived
//!   through the bridge). An SMS message is MANAGED by construction (CHECK): carrier
//!   plaintext can never be presented as end-to-end encrypted.
//! * `messaging_sms_bridges` — a MANAGED conversation ↔ one external party reachable by
//!   SMS on one number resource. Messaging stores the party's TELEPHONE_ENDPOINT principal
//!   and the MASKED number only; the number itself lives sealed in the SMS Gateway. A
//!   trigger refuses a bridge on a PRIVATE_E2EE conversation (never an automatic
//!   downgrade); the explicit disclosure version the creator acknowledged is recorded.
//! * `messaging_sms_transports` — the CARRIER state of each bridged message, kept apart
//!   from in-app delivery / read state (§110).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_messages
    ADD COLUMN channel VARCHAR(8) NOT NULL DEFAULT 'IN_APP',
    ADD CONSTRAINT ck_messaging_messages_channel CHECK (channel IN ('IN_APP', 'SMS')),
    ADD CONSTRAINT ck_messaging_messages_sms_managed CHECK (channel = 'IN_APP' OR security_profile = 'MANAGED');

CREATE TABLE messaging_sms_bridges (
    id                       UUID         NOT NULL,
    tenant_id                UUID         NOT NULL,
    application_id           UUID         NOT NULL,
    environment_id           UUID         NOT NULL,
    environment_kind         VARCHAR(16)  NOT NULL,
    service_account_id       UUID         NOT NULL,
    conversation_id          UUID         NOT NULL,
    number_resource_id       UUID         NOT NULL,
    external_principal_id    UUID         NOT NULL,
    remote_masked            VARCHAR(32)  NOT NULL,
    status                   VARCHAR(16)  NOT NULL DEFAULT 'ACTIVE',
    disclosure_version       VARCHAR(32)  NOT NULL,
    created_by_principal_id  UUID         NOT NULL,
    created_at               TIMESTAMPTZ  NOT NULL,
    closed_at                TIMESTAMPTZ,
    CONSTRAINT pk_messaging_sms_bridges PRIMARY KEY (id),
    CONSTRAINT fk_messaging_sms_bridges_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_sms_bridges_status CHECK (status IN ('ACTIVE', 'CLOSED')),
    CONSTRAINT ck_messaging_sms_bridges_closed CHECK ((status = 'CLOSED') = (closed_at IS NOT NULL)),
    CONSTRAINT ck_messaging_sms_bridges_masked CHECK (remote_masked !~ '[0-9]{5,}')
);
CREATE UNIQUE INDEX uq_messaging_sms_bridges_active ON messaging_sms_bridges (conversation_id) WHERE status = 'ACTIVE';
CREATE FUNCTION messaging_sms_bridges_managed_only() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT security_profile FROM messaging_conversations WHERE id = NEW.conversation_id) <> 'MANAGED' THEN
        RAISE EXCEPTION 'messaging: an SMS bridge needs a MANAGED conversation (never an E2EE downgrade)' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_sms_bridges_managed_only BEFORE INSERT ON messaging_sms_bridges
    FOR EACH ROW EXECUTE FUNCTION messaging_sms_bridges_managed_only();

CREATE TABLE messaging_sms_transports (
    message_id        UUID         NOT NULL,
    bridge_id         UUID         NOT NULL,
    direction         VARCHAR(8)   NOT NULL,
    sms_message_id    UUID,
    status            VARCHAR(16)  NOT NULL,
    blocked_reason    VARCHAR(64),
    failure_code      VARCHAR(64),
    segment_count     INTEGER,
    attempt_count     INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at   TIMESTAMPTZ,
    created_at        TIMESTAMPTZ  NOT NULL,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_sms_transports PRIMARY KEY (message_id),
    CONSTRAINT fk_messaging_sms_transports_bridge FOREIGN KEY (bridge_id) REFERENCES messaging_sms_bridges (id),
    CONSTRAINT uq_messaging_sms_transports_sms UNIQUE (sms_message_id),
    CONSTRAINT ck_messaging_sms_transports_direction CHECK (direction IN ('OUTBOUND', 'INBOUND')),
    CONSTRAINT ck_messaging_sms_transports_status CHECK (status IN ('QUEUED', 'REQUESTED', 'AUTHORIZED', 'SUBMITTING',
        'SUBMITTED', 'DELIVERED', 'FAILED', 'UNKNOWN', 'EXPIRED', 'BLOCKED', 'RECEIVED'))
);
CREATE INDEX ix_messaging_sms_transports_due ON messaging_sms_transports (next_attempt_at)
    WHERE next_attempt_at IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_sms_transports;
DROP TABLE IF EXISTS messaging_sms_bridges;
DROP FUNCTION IF EXISTS messaging_sms_bridges_managed_only();
ALTER TABLE messaging_messages DROP CONSTRAINT IF EXISTS ck_messaging_messages_sms_managed,
    DROP CONSTRAINT IF EXISTS ck_messaging_messages_channel, DROP COLUMN IF EXISTS channel;
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
