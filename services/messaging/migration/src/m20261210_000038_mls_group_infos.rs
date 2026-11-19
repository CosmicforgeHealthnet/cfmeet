//! MLS self-join (additive; its own `messaging_mls_join_seaql_migrations` history).
//!
//! `messaging_mls_group_infos`: the latest MLS `GroupInfo` (RFC 9420 §12.4.3) of each
//! PRIVATE_E2EE group, as a member device published it with its commit. A device of a
//! member joins the group BY ITSELF from it (an external commit, §12.4.3.2) — no other
//! member has to be online. A GroupInfo is PUBLIC material signed by a member (ratchet
//! tree public keys, credentials, group context, the external HPKE public key); it holds
//! no private key, group secret or plaintext. It is served only to ACTIVE members, and only
//! while its epoch is the group's current one.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_mls_group_infos (
    conversation_id      UUID         NOT NULL,
    mls_epoch            BIGINT       NOT NULL,
    group_info           BYTEA        NOT NULL,
    publisher_device_id  UUID         NOT NULL,
    updated_at           TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_mls_group_infos PRIMARY KEY (conversation_id),
    CONSTRAINT fk_messaging_mls_group_infos_conversation FOREIGN KEY (conversation_id)
        REFERENCES messaging_conversations (id),
    CONSTRAINT ck_messaging_mls_group_infos_epoch CHECK (mls_epoch >= 0 AND mls_epoch <= 9007199254740992),
    CONSTRAINT ck_messaging_mls_group_infos_size CHECK (octet_length(group_info) BETWEEN 1 AND 1048576)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_mls_group_infos;
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
