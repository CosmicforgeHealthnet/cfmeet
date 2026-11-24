//! Independent, additive history relay. Older application images keep their original
//! migration histories; no existing message, device or v1 transfer row is rewritten.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(r#"
CREATE TABLE messaging_history_transfers_v2 (
 id UUID PRIMARY KEY,
 tenant_id UUID NOT NULL,
 application_id UUID NOT NULL,
 environment_id UUID NOT NULL,
 principal_id UUID NOT NULL,
 source_device_id UUID NOT NULL REFERENCES messaging_devices(id),
 target_device_id UUID NOT NULL REFERENCES messaging_devices(id),
 transfer_public_key BYTEA NOT NULL CHECK (octet_length(transfer_public_key)=32),
 request_signature BYTEA NOT NULL CHECK (octet_length(request_signature)=64),
 status VARCHAR(16) NOT NULL CHECK (status IN ('REQUESTED','SEALED','COMPLETED','CANCELLED','EXPIRED')),
 snapshot_id UUID,
 manifest_digest BYTEA CHECK (manifest_digest IS NULL OR octet_length(manifest_digest)=32),
 chunk_count INTEGER CHECK (chunk_count BETWEEN 1 AND 4096),
 total_decoded_bytes BIGINT CHECK (total_decoded_bytes BETWEEN 1 AND 1073741824),
 sealed_manifest BYTEA CHECK (sealed_manifest IS NULL OR octet_length(sealed_manifest)<=262265),
 stored_bytes BIGINT NOT NULL DEFAULT 0 CHECK (stored_bytes BETWEEN 0 AND 1074500000),
 next_index INTEGER NOT NULL DEFAULT 0 CHECK (next_index>=0),
 end_reason VARCHAR(32),
 created_at TIMESTAMPTZ NOT NULL,
 expires_at TIMESTAMPTZ NOT NULL,
 ended_at TIMESTAMPTZ,
 CHECK (source_device_id<>target_device_id),
 CHECK (expires_at>created_at),
 CHECK ((snapshot_id IS NULL)=(manifest_digest IS NULL)),
 CHECK ((snapshot_id IS NULL)=(chunk_count IS NULL)),
 CHECK ((snapshot_id IS NULL)=(total_decoded_bytes IS NULL)),
 CHECK (chunk_count IS NULL OR next_index<=chunk_count)
);
CREATE UNIQUE INDEX uq_history_v2_open_target ON messaging_history_transfers_v2(target_device_id)
 WHERE status IN ('REQUESTED','SEALED');
CREATE INDEX ix_history_v2_expiry ON messaging_history_transfers_v2(expires_at)
 WHERE status IN ('REQUESTED','SEALED');
CREATE TABLE messaging_history_transfer_chunks_v2 (
 transfer_id UUID NOT NULL REFERENCES messaging_history_transfers_v2(id) ON DELETE CASCADE,
 chunk_index INTEGER NOT NULL CHECK (chunk_index BETWEEN 0 AND 4095),
 sealed_chunk BYTEA NOT NULL CHECK (octet_length(sealed_chunk) BETWEEN 121 AND 262265),
 sealed_sha256 BYTEA NOT NULL CHECK (octet_length(sealed_sha256)=32),
 PRIMARY KEY(transfer_id,chunk_index)
);
"#).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping a relay while interrupted clients still own checkpoints is not an
        // automatic rollback. Disable capability/configuration and let transfers expire.
        Err(DbErr::Custom(
            "history relay v2 is additive; disable it instead of deleting pending transfers".into(),
        ))
    }
}
