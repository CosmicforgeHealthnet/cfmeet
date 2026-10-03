use sea_orm_migration::prelude::*;

use super::m20260116_000001_create_users::Users;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ENUM type first
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE meeting_status AS ENUM ('scheduled', 'ongoing', 'ended', 'cancelled');"
            )
            .await?;

        // Create table
        manager
            .create_table(
                Table::create()
                    .table(Meetings::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Meetings::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(
                        ColumnDef::new(Meetings::MeetingIdentifier)
                            .string_len(50)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Meetings::UserId).uuid())
                    .col(ColumnDef::new(Meetings::HostId).uuid().not_null())
                    .col(ColumnDef::new(Meetings::Title).string_len(255).not_null())
                    .col(ColumnDef::new(Meetings::Metadata).json())
                    .col(
                        ColumnDef::new(Meetings::IsPrivate)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Meetings::StartTime).timestamp().not_null())
                    .col(ColumnDef::new(Meetings::EndTime).timestamp())
                    .col(
                        ColumnDef::new(Meetings::Status)
                            .custom(Alias::new("meeting_status"))
                            .not_null()
                            .default("scheduled"),
                    )
                    .col(
                        ColumnDef::new(Meetings::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Meetings::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_meetings_user")
                            .from(Meetings::Table, Meetings::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_meetings_host")
                            .from(Meetings::Table, Meetings::HostId)
                            .to(Users::Table, Users::Id)
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
                    .name("idx_meetings_user_id")
                    .table(Meetings::Table)
                    .col(Meetings::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_meetings_host_id")
                    .table(Meetings::Table)
                    .col(Meetings::HostId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_meetings_identifier")
                    .table(Meetings::Table)
                    .col(Meetings::MeetingIdentifier)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_meetings_status")
                    .table(Meetings::Table)
                    .col(Meetings::Status)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Meetings::Table).to_owned())
            .await?;

        // Drop ENUM type
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS meeting_status;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum Meetings {
    Table,
    Id,
    MeetingIdentifier,
    UserId,
    HostId,
    Title,
    Metadata,
    IsPrivate,
    StartTime,
    EndTime,
    Status,
    CreatedAt,
    UpdatedAt,
}
