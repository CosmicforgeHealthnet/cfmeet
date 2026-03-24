use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(OauthStates::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OauthStates::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(
                        ColumnDef::new(OauthStates::State)
                            .string_len(255)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(OauthStates::Provider)
                            .string_len(50)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OauthStates::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(OauthStates::ExpiresAt)
                            .timestamp()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Create indexes for faster lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_oauth_states_state")
                    .table(OauthStates::Table)
                    .col(OauthStates::State)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_oauth_states_expires_at")
                    .table(OauthStates::Table)
                    .col(OauthStates::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OauthStates::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum OauthStates {
    Table,
    Id,
    State,
    Provider,
    CreatedAt,
    ExpiresAt,
}
