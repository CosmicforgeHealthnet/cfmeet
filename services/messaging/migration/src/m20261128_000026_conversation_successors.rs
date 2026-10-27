//! Part V Appendix G (G-29): a conversation's security profile never changes; moving to
//! another profile is a deliberately created SUCCESSOR conversation with user-visible
//! disclosure and provenance.
//!
//! * `predecessor_conversation_id` — set at creation, never changed.
//! * `successor_conversation_id` — set once on the predecessor (one successor each).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE messaging_conversations
    ADD COLUMN predecessor_conversation_id UUID,
    ADD COLUMN successor_conversation_id UUID,
    ADD CONSTRAINT fk_messaging_conversations_predecessor FOREIGN KEY (predecessor_conversation_id) REFERENCES messaging_conversations (id),
    ADD CONSTRAINT fk_messaging_conversations_successor FOREIGN KEY (successor_conversation_id) REFERENCES messaging_conversations (id),
    ADD CONSTRAINT ck_messaging_conversations_not_own_predecessor CHECK (predecessor_conversation_id IS DISTINCT FROM id AND successor_conversation_id IS DISTINCT FROM id);
CREATE UNIQUE INDEX uq_messaging_conversations_predecessor ON messaging_conversations (predecessor_conversation_id)
    WHERE predecessor_conversation_id IS NOT NULL;

CREATE FUNCTION messaging_conversation_provenance_guard() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.predecessor_conversation_id IS DISTINCT FROM OLD.predecessor_conversation_id THEN
        RAISE EXCEPTION 'messaging: a conversation''s predecessor is immutable' USING ERRCODE = 'P0001';
    END IF;
    IF OLD.successor_conversation_id IS NOT NULL
       AND NEW.successor_conversation_id IS DISTINCT FROM OLD.successor_conversation_id THEN
        RAISE EXCEPTION 'messaging: a conversation''s successor is set once' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_conversation_provenance_guard
    BEFORE UPDATE OF predecessor_conversation_id, successor_conversation_id ON messaging_conversations
    FOR EACH ROW EXECUTE FUNCTION messaging_conversation_provenance_guard();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS trg_messaging_conversation_provenance_guard ON messaging_conversations;
DROP FUNCTION IF EXISTS messaging_conversation_provenance_guard();
DROP INDEX IF EXISTS uq_messaging_conversations_predecessor;
ALTER TABLE messaging_conversations
    DROP CONSTRAINT IF EXISTS ck_messaging_conversations_not_own_predecessor,
    DROP CONSTRAINT IF EXISTS fk_messaging_conversations_successor,
    DROP CONSTRAINT IF EXISTS fk_messaging_conversations_predecessor,
    DROP COLUMN IF EXISTS successor_conversation_id,
    DROP COLUMN IF EXISTS predecessor_conversation_id;
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
