//! Part V §46 / §121 — MANAGED edits keep their prior versions (`message_revisions`).
//!
//! Before this migration a MANAGED edit replaced the sealed text in place, destroying the
//! previous version even under a legal hold. Every edit now first copies the version it
//! supersedes into `messaging_message_revisions` (still sealed under the managed key ring
//! with the message's own scope: the copy is ciphertext the database cannot read).
//!
//! * Rows are append-only: never updated, and deleted only together with a message that
//!   has been PURGED (retention purge removes a message's revisions in the same
//!   transaction, after its legal-hold check). A held message therefore keeps every prior
//!   version.
//! * Only MANAGED messages have revisions; PRIVATE_E2EE edits are separate ciphertext
//!   messages and the server never holds their content.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_message_revisions (
    message_id             UUID         NOT NULL,
    revision               INTEGER      NOT NULL,
    environment_id         UUID         NOT NULL,
    conversation_id        UUID         NOT NULL,
    edited_by_principal_id UUID         NOT NULL,
    managed_sealed         BYTEA        NOT NULL,
    managed_seal_nonce     BYTEA        NOT NULL,
    managed_seal_key_id    VARCHAR(32)  NOT NULL,
    created_at             TIMESTAMPTZ  NOT NULL,
    superseded_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_message_revisions PRIMARY KEY (message_id, revision),
    CONSTRAINT fk_messaging_message_revisions_message FOREIGN KEY (message_id) REFERENCES messaging_messages (id),
    CONSTRAINT ck_messaging_message_revisions_revision CHECK (revision >= 0)
);
CREATE INDEX ix_messaging_message_revisions_conversation ON messaging_message_revisions (conversation_id);

CREATE FUNCTION messaging_message_revisions_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
DECLARE
    parent_profile TEXT;
    parent_lifecycle TEXT;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'messaging: message revisions are immutable' USING ERRCODE = 'P0001';
    END IF;
    IF TG_OP = 'INSERT' THEN
        SELECT security_profile, lifecycle INTO parent_profile, parent_lifecycle
          FROM messaging_messages WHERE id = NEW.message_id;
        IF parent_profile IS DISTINCT FROM 'MANAGED' THEN
            RAISE EXCEPTION 'messaging: only MANAGED messages have server-side revisions' USING ERRCODE = 'P0001';
        END IF;
        RETURN NEW;
    END IF;
    SELECT lifecycle INTO parent_lifecycle FROM messaging_messages WHERE id = OLD.message_id;
    IF parent_lifecycle IS DISTINCT FROM 'PURGED' THEN
        RAISE EXCEPTION 'messaging: a revision is removed only with its purged message' USING ERRCODE = 'P0001';
    END IF;
    RETURN OLD;
END $$;
CREATE TRIGGER trg_messaging_message_revisions_guard
    BEFORE INSERT OR UPDATE OR DELETE ON messaging_message_revisions
    FOR EACH ROW EXECUTE FUNCTION messaging_message_revisions_guard();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_message_revisions;
DROP FUNCTION IF EXISTS messaging_message_revisions_guard();
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
