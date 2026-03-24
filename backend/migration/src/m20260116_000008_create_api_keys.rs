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
            .execute_unprepared("CREATE TYPE api_key_status AS ENUM ('active', 'revoked');")
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(ApiKeys::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ApiKeys::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(ColumnDef::new(ApiKeys::UserId).uuid().not_null())
                    .col(
                        ColumnDef::new(ApiKeys::ApiKey)
                            .string_len(64)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(ApiKeys::UsageLimit)
                            .integer()
                            .not_null()
                            .default(1000),
                    )
                    .col(
                        ColumnDef::new(ApiKeys::UsedCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(ApiKeys::ExpiresAt).timestamp().not_null())
                    .col(
                        ColumnDef::new(ApiKeys::Status)
                            .custom(Alias::new("api_key_status"))
                            .not_null()
                            .default("active"),
                    )
                    .col(
                        ColumnDef::new(ApiKeys::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(ApiKeys::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_api_keys_user")
                            .from(ApiKeys::Table, ApiKeys::UserId)
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
                    .name("idx_api_keys_user_id")
                    .table(ApiKeys::Table)
                    .col(ApiKeys::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_api_keys_api_key")
                    .table(ApiKeys::Table)
                    .col(ApiKeys::ApiKey)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_api_keys_status")
                    .table(ApiKeys::Table)
                    .col(ApiKeys::Status)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ApiKeys::Table).to_owned())
            .await?;

        // Drop ENUM type
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS api_key_status;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum ApiKeys {
    Table,
    Id,
    UserId,
    ApiKey,
    UsageLimit,
    UsedCount,
    ExpiresAt,
    Status,
    CreatedAt,
    UpdatedAt,
}
