use sea_orm_migration::prelude::*;

use super::m20260116_000001_create_users::Users;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ENUM types first
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE webhook_event_type AS ENUM ('meeting_start', 'meeting_end', 'participant_join', 'participant_leave');"
            )
            .await?;

        manager
            .get_connection()
            .execute_unprepared("CREATE TYPE webhook_status AS ENUM ('active', 'inactive');")
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Webhooks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Webhooks::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(ColumnDef::new(Webhooks::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(Webhooks::EventType)
                            .custom(Alias::new("webhook_event_type"))
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Webhooks::EndpointUrl)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(ColumnDef::new(Webhooks::Secret).string_len(64).not_null())
                    .col(
                        ColumnDef::new(Webhooks::Status)
                            .custom(Alias::new("webhook_status"))
                            .not_null()
                            .default("active"),
                    )
                    .col(
                        ColumnDef::new(Webhooks::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Webhooks::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_webhooks_user")
                            .from(Webhooks::Table, Webhooks::UserId)
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
                    .name("idx_webhooks_user_id")
                    .table(Webhooks::Table)
                    .col(Webhooks::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_webhooks_event_type")
                    .table(Webhooks::Table)
                    .col(Webhooks::EventType)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Webhooks::Table).to_owned())
            .await?;

        // Drop ENUM types
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS webhook_event_type;")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS webhook_status;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum Webhooks {
    Table,
    Id,
    UserId,
    EventType,
    EndpointUrl,
    Secret,
    Status,
    CreatedAt,
    UpdatedAt,
}
