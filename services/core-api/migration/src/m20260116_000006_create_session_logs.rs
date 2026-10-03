use sea_orm_migration::prelude::*;

use super::m20260116_000002_create_meetings::Meetings;
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
                "CREATE TYPE event_type AS ENUM ('meeting_start', 'meeting_end', 'participant_join', 'participant_leave', 'role_change', 'screen_share_start', 'screen_share_end');"
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(SessionLogs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionLogs::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(ColumnDef::new(SessionLogs::MeetingId).uuid().not_null())
                    .col(ColumnDef::new(SessionLogs::ParticipantId).uuid())
                    .col(
                        ColumnDef::new(SessionLogs::EventType)
                            .custom(Alias::new("event_type"))
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionLogs::EventTime)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(SessionLogs::Metadata).json())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_logs_meeting")
                            .from(SessionLogs::Table, SessionLogs::MeetingId)
                            .to(Meetings::Table, Meetings::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_logs_participant")
                            .from(SessionLogs::Table, SessionLogs::ParticipantId)
                            .to(Participants::Table, Participants::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes
        manager
            .create_index(
                Index::create()
                    .name("idx_logs_meeting_id")
                    .table(SessionLogs::Table)
                    .col(SessionLogs::MeetingId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_logs_event_time")
                    .table(SessionLogs::Table)
                    .col(SessionLogs::EventTime)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_logs_event_type")
                    .table(SessionLogs::Table)
                    .col(SessionLogs::EventType)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SessionLogs::Table).to_owned())
            .await?;

        // Drop ENUM type
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS event_type;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum SessionLogs {
    Table,
    Id,
    MeetingId,
    ParticipantId,
    EventType,
    EventTime,
    Metadata,
}
