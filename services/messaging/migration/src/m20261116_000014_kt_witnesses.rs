//! Part V remediation — key-transparency witness cosignatures (AD-05 interface).
//!
//! * `messaging_transparency_witness_state` — per configured witness, the tree size and
//!   root it last cosigned (what the next submission must prove consistency from).
//! * `messaging_transparency_cosignatures` — every cosignature a witness returned AND
//!   Messaging verified under the witness's pinned key; append-only. Served with the
//!   tree head it covers so clients can require a quorum of independent witnesses.
//!
//! Only public material: sizes, root hashes, timestamps and Ed25519 signatures.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_transparency_witness_state (
    witness_id    VARCHAR(64)  NOT NULL,
    tree_size     BIGINT       NOT NULL,
    root_hash     BYTEA        NOT NULL,
    updated_at    TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_transparency_witness_state PRIMARY KEY (witness_id),
    CONSTRAINT ck_messaging_transparency_witness_state_id CHECK (witness_id ~ '^[a-z0-9][a-z0-9.:-]{2,63}$'),
    CONSTRAINT ck_messaging_transparency_witness_state_size CHECK (tree_size >= 0),
    CONSTRAINT ck_messaging_transparency_witness_state_root CHECK (octet_length(root_hash) = 32)
);

CREATE TABLE messaging_transparency_cosignatures (
    witness_id             VARCHAR(64)  NOT NULL,
    tree_size              BIGINT       NOT NULL,
    root_hash              BYTEA        NOT NULL,
    head_timestamp_micros  BIGINT       NOT NULL,
    cosigned_at_micros     BIGINT       NOT NULL,
    signature              BYTEA        NOT NULL,
    created_at             TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_transparency_cosignatures PRIMARY KEY (witness_id, tree_size),
    CONSTRAINT ck_messaging_transparency_cosignatures_id CHECK (witness_id ~ '^[a-z0-9][a-z0-9.:-]{2,63}$'),
    CONSTRAINT ck_messaging_transparency_cosignatures_size CHECK (tree_size >= 0),
    CONSTRAINT ck_messaging_transparency_cosignatures_root CHECK (octet_length(root_hash) = 32),
    CONSTRAINT ck_messaging_transparency_cosignatures_sig CHECK (octet_length(signature) = 64)
);
CREATE TRIGGER trg_messaging_transparency_cosignatures_append_only BEFORE UPDATE OR DELETE ON messaging_transparency_cosignatures
    FOR EACH ROW EXECUTE FUNCTION messaging_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_transparency_cosignatures;
DROP TABLE IF EXISTS messaging_transparency_witness_state;
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
