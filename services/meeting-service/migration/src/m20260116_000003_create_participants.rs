use sea_orm_migration::prelude::*;

use super::m20260116_000001_create_users::Users;
use super::m20260116_000002_create_meetings::Meetings;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ENUM types first
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE participant_role AS ENUM ('host', 'participant', 'viewer');",
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE participant_status AS ENUM ('waiting', 'joined', 'kicked');",
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Participants::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Participants::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(ColumnDef::new(Participants::MeetingId).uuid().not_null())
                    .col(ColumnDef::new(Participants::UserId).uuid())
                    .col(
                        ColumnDef::new(Participants::Role)
                            .custom(Alias::new("participant_role"))
                            .not_null()
                            .default("participant"),
                    )
                    .col(
                        ColumnDef::new(Participants::JoinTime)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Participants::LeaveTime).timestamp())
                    .col(
                        ColumnDef::new(Participants::Status)
                            .custom(Alias::new("participant_status"))
                            .not_null()
                            .default("waiting"),
                    )
                    .col(
                        ColumnDef::new(Participants::IsMuted)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Participants::IsVideoOn)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(Participants::IsScreenSharing)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Participants::DisplayName)
                            .string_len(100)
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_participants_meeting")
                            .from(Participants::Table, Participants::MeetingId)
                            .to(Meetings::Table, Meetings::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_participants_user")
                            .from(Participants::Table, Participants::UserId)
                            .to(Users::Table, Users::Id)
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
                    .name("idx_participants_meeting_id")
                    .table(Participants::Table)
                    .col(Participants::MeetingId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_participants_user_id")
                    .table(Participants::Table)
                    .col(Participants::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_participants_status")
                    .table(Participants::Table)
                    .col(Participants::Status)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Participants::Table).to_owned())
            .await?;

        // Drop ENUM types
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS participant_role;")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS participant_status;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum Participants {
    Table,
    Id,
    MeetingId,
    UserId,
    Role,
    JoinTime,
    LeaveTime,
    Status,
    IsMuted,
    IsVideoOn,
    IsScreenSharing,
    DisplayName,
}
