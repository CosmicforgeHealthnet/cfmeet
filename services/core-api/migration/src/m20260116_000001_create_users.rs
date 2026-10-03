use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create ENUM types first
        manager
            .get_connection()
            .execute_unprepared("CREATE TYPE auth_type AS ENUM ('local', 'google');")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("CREATE TYPE user_role AS ENUM ('user', 'admin');")
            .await?;

        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE user_status AS ENUM ('pending_verification', 'active', 'inactive');",
            )
            .await?;

        // Create table
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Users::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(
                        ColumnDef::new(Users::Username)
                            .string_len(50)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Users::Email)
                            .string_len(100)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Users::PasswordHash).string_len(255))
                    .col(
                        ColumnDef::new(Users::AuthType)
                            .custom(Alias::new("auth_type"))
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(Users::Role)
                            .custom(Alias::new("user_role"))
                            .not_null()
                            .default("user"),
                    )
                    .col(
                        ColumnDef::new(Users::Status)
                            .custom(Alias::new("user_status"))
                            .not_null()
                            .default("pending_verification"),
                    )
                    .col(ColumnDef::new(Users::VerificationToken).string_len(128))
                    .col(ColumnDef::new(Users::TokenExpiresAt).timestamp())
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Users::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Users::LastLogin).timestamp())
                    .to_owned(),
            )
            .await?;

        // Create indexes for better query performance
        manager
            .create_index(
                Index::create()
                    .name("idx_users_email")
                    .table(Users::Table)
                    .col(Users::Email)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_users_username")
                    .table(Users::Table)
                    .col(Users::Username)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await?;

        // Drop ENUM types
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS user_status;")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS user_role;")
            .await?;

        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS auth_type;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum Users {
    Table,
    Id,
    Username,
    Email,
    PasswordHash,
    AuthType,
    Role,
    Status,
    VerificationToken,
    TokenExpiresAt,
    CreatedAt,
    UpdatedAt,
    LastLogin,
}
