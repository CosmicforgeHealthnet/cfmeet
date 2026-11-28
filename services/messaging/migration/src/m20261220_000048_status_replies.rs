//! Status replies for their author (additive; the FIRST migration of the separate
//! `messaging_status_replies_seaql_migrations` history, which no released image reads, so a
//! rollback to the previous image still boots on this schema: that image never writes or
//! reads these rows, and the column it does not know is nullable).
//!
//! * `messaging_status_replies` — one row per private reply to a status: WHICH chat message
//!   answered WHICH status, so the author's "My status" lists the replies of each status
//!   and jumps to the chat. No content: the reply is the MANAGED direct message it points
//!   at (sealed at rest like every message); its text is read through the normal member
//!   read when the author lists the replies, so a deleted message stops being listed. The
//!   row goes with its status (foreign key, `ON DELETE CASCADE`: deletion and the 24-hour
//!   sweep) and carries the status's `expires_at` for the sweep's own pass.
//! * `messaging_status_views.reacted_at` — when the viewer's current reaction was set (the
//!   chat list's "Reacted to your status" preview orders it against the chat's messages).
//!
//! Principals, tenants, applications and environments are opaque Platform Access ids.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_status_replies (
    id                    UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    status_id             UUID         NOT NULL,
    author_principal_id   UUID         NOT NULL,
    replier_principal_id  UUID         NOT NULL,
    conversation_id       UUID         NOT NULL,
    message_id            UUID         NOT NULL,
    created_at            TIMESTAMPTZ  NOT NULL,
    expires_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_status_replies PRIMARY KEY (id),
    CONSTRAINT uq_messaging_status_replies_message UNIQUE (message_id),
    CONSTRAINT fk_messaging_status_replies_status FOREIGN KEY (status_id) REFERENCES messaging_statuses (id) ON DELETE CASCADE
);
CREATE INDEX ix_messaging_status_replies_status ON messaging_status_replies (status_id, created_at DESC, id DESC);
CREATE INDEX ix_messaging_status_replies_expiry ON messaging_status_replies (expires_at);

ALTER TABLE messaging_status_views ADD COLUMN IF NOT EXISTS reacted_at TIMESTAMPTZ;
"#;

const DOWN: &str = r#"
ALTER TABLE messaging_status_views DROP COLUMN IF EXISTS reacted_at;
DROP TABLE IF EXISTS messaging_status_replies;
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
