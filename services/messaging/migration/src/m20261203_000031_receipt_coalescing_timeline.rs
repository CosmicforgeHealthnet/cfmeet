//! Audit closure MSG-018 / MSG-009 — receipt-event coalescing and Hub timeline nudges.
//!
//! * `messaging_receipt_emissions` (MSG-018): per conversation, member and receipt kind
//!   (`READ`, `DELIVERED`), the cursor last published to the conversation's event stream
//!   and, inside the coalescing window, the highest cursor still to publish. The first
//!   advance of a window is published at once; later advances inside the window only
//!   raise `pending_sequence`, and one event with the highest cursor is published when
//!   the window ends (`due_at`). Receipt traffic therefore no longer takes the
//!   conversation's event-sequence row lock on every read / acknowledgement. The
//!   canonical cursors stay in `messaging_read_cursors` / `messaging_received_cursors`.
//! * `messaging_conversation_timeline_versions` (MSG-009): the highest Communications Hub
//!   timeline version announced in a conversation's event stream, so a redelivered or
//!   out-of-order Hub nudge never publishes a second (or an older) `timeline.changed`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_receipt_emissions (
    conversation_id   UUID         NOT NULL,
    principal_id      UUID         NOT NULL,
    receipt_kind      VARCHAR(16)  NOT NULL,
    tenant_id         UUID         NOT NULL,
    application_id    UUID         NOT NULL,
    environment_id    UUID         NOT NULL,
    emitted_sequence  BIGINT       NOT NULL DEFAULT 0,
    emitted_at        TIMESTAMPTZ  NOT NULL DEFAULT '-infinity',
    pending_sequence  BIGINT,
    due_at            TIMESTAMPTZ,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_receipt_emissions PRIMARY KEY (conversation_id, principal_id, receipt_kind),
    CONSTRAINT fk_messaging_receipt_emissions_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_receipt_emissions_kind CHECK (receipt_kind IN ('READ', 'DELIVERED')),
    CONSTRAINT ck_messaging_receipt_emissions_emitted CHECK (emitted_sequence >= 0),
    CONSTRAINT ck_messaging_receipt_emissions_pending CHECK (
        (pending_sequence IS NULL AND due_at IS NULL)
        OR (pending_sequence IS NOT NULL AND due_at IS NOT NULL AND pending_sequence > emitted_sequence))
);
CREATE INDEX ix_messaging_receipt_emissions_due ON messaging_receipt_emissions (due_at) WHERE due_at IS NOT NULL;

CREATE FUNCTION messaging_receipt_emissions_monotonic() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.emitted_sequence < OLD.emitted_sequence THEN
        RAISE EXCEPTION 'messaging: a published receipt cursor never moves back' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_receipt_emissions_monotonic BEFORE UPDATE ON messaging_receipt_emissions
    FOR EACH ROW EXECUTE FUNCTION messaging_receipt_emissions_monotonic();

CREATE TABLE messaging_conversation_timeline_versions (
    conversation_id   UUID         NOT NULL,
    timeline_version  BIGINT       NOT NULL,
    updated_at        TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_conversation_timeline_versions PRIMARY KEY (conversation_id),
    CONSTRAINT fk_messaging_conversation_timeline_versions_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_conversation_timeline_versions_value CHECK (timeline_version >= 1)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_conversation_timeline_versions;
DROP TABLE IF EXISTS messaging_receipt_emissions;
DROP FUNCTION IF EXISTS messaging_receipt_emissions_monotonic();
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
