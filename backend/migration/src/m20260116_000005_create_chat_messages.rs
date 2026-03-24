use sea_orm_migration::prelude::*;

use super::m20260116_000002_create_meetings::Meetings;
use super::m20260116_000003_create_participants::Participants;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ChatMessages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ChatMessages::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(ColumnDef::new(ChatMessages::MeetingId).uuid().not_null())
                    .col(
                        ColumnDef::new(ChatMessages::ParticipantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(ChatMessages::Message).text().not_null())
                    .col(
                        ColumnDef::new(ChatMessages::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_chat_meeting")
                            .from(ChatMessages::Table, ChatMessages::MeetingId)
                            .to(Meetings::Table, Meetings::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_chat_participant")
                            .from(ChatMessages::Table, ChatMessages::ParticipantId)
                            .to(Participants::Table, Participants::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_chat_meeting_id")
                    .table(ChatMessages::Table)
                    .col(ChatMessages::MeetingId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_chat_created_at")
                    .table(ChatMessages::Table)
                    .col(ChatMessages::CreatedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ChatMessages::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum ChatMessages {
    Table,
    Id,
    MeetingId,
    ParticipantId,
    Message,
    CreatedAt,
}
