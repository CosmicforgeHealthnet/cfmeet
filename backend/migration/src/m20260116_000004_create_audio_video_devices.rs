use sea_orm_migration::prelude::*;

use super::m20260116_000003_create_participants::Participants;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ENUM type first
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE device_type AS ENUM ('audio_input', 'audio_output', 'video_input');",
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(AudioVideoDevices::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AudioVideoDevices::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(
                        ColumnDef::new(AudioVideoDevices::ParticipantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AudioVideoDevices::DeviceType)
                            .custom(Alias::new("device_type"))
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AudioVideoDevices::DeviceName)
                            .string_len(100)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AudioVideoDevices::IsActive)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(AudioVideoDevices::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_devices_participant")
                            .from(AudioVideoDevices::Table, AudioVideoDevices::ParticipantId)
                            .to(Participants::Table, Participants::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create index
        manager
            .create_index(
                Index::create()
                    .name("idx_devices_participant_id")
                    .table(AudioVideoDevices::Table)
                    .col(AudioVideoDevices::ParticipantId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AudioVideoDevices::Table).to_owned())
            .await?;

        // Drop ENUM type
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS device_type;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum AudioVideoDevices {
    Table,
    Id,
    ParticipantId,
    DeviceType,
    DeviceName,
    IsActive,
    CreatedAt,
}
