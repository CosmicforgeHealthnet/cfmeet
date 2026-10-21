//! Part V §52 / §118 — recipient DELIVERED state (`messaging.delivery-cursor.updated`).
//!
//! A message is DELIVERED to a recipient principal once at least one of its authorized
//! client sessions acknowledged receiving it, or the principal synchronized past it (its
//! read cursor). `messaging_delivery_cursors` keeps, per member, the highest message
//! sequence so delivered. It only grows (enforced here), like the read cursor.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_delivery_cursors (
    conversation_id     UUID         NOT NULL,
    principal_id        UUID         NOT NULL,
    delivered_sequence  BIGINT       NOT NULL,
    updated_at          TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_delivery_cursors PRIMARY KEY (conversation_id, principal_id),
    CONSTRAINT fk_messaging_delivery_cursors_conversation FOREIGN KEY (conversation_id) REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_delivery_cursors_value CHECK (delivered_sequence >= 0)
);

CREATE FUNCTION messaging_delivery_cursors_monotonic() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.delivered_sequence < OLD.delivered_sequence THEN
        RAISE EXCEPTION 'messaging: a delivery cursor never moves back' USING ERRCODE = 'P0001';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER trg_messaging_delivery_cursors_monotonic BEFORE UPDATE ON messaging_delivery_cursors
    FOR EACH ROW EXECUTE FUNCTION messaging_delivery_cursors_monotonic();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_delivery_cursors;
DROP FUNCTION IF EXISTS messaging_delivery_cursors_monotonic();
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
