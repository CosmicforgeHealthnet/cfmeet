//! Migration: Migrate OTP, OAuth states, and email queue to Redis
//!
//! This migration removes database-stored OTP tokens, OAuth states, and email jobs
//! as they are now handled by Redis for better performance and automatic expiration.
//!
//! Changes:
//! - Remove verification_token and token_expires_at from users table
//! - Remove reset_token and reset_token_expires_at from users table
//! - Drop oauth_states table (now using Redis with auto-expiry)
//! - Drop email_jobs table (now using Redis Streams)

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Remove OTP columns from users table
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .drop_column(Users::VerificationToken)
                    .drop_column(Users::TokenExpiresAt)
                    .drop_column(Users::ResetToken)
                    .drop_column(Users::ResetTokenExpiresAt)
                    .to_owned(),
            )
            .await?;

        // 2. Drop oauth_states table (now using Redis)
        manager
            .drop_table(Table::drop().table(OauthStates::Table).to_owned())
            .await?;

        // 3. Drop email_jobs table (now using Redis Streams)
        manager
            .drop_table(Table::drop().table(EmailJobs::Table).to_owned())
            .await?;

        // 4. Drop email_job_status enum type
        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS email_job_status;")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Re-add OTP columns to users table
        manager
            .alter_table(
                Table::alter()
                    .table(Users::Table)
                    .add_column(ColumnDef::new(Users::VerificationToken).string().null())
                    .add_column(ColumnDef::new(Users::TokenExpiresAt).timestamp().null())
                    .add_column(ColumnDef::new(Users::ResetToken).string().null())
                    .add_column(ColumnDef::new(Users::ResetTokenExpiresAt).timestamp().null())
                    .to_owned(),
            )
            .await?;

        // 2. Recreate oauth_states table
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

        // 3. Recreate email_job_status enum
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE email_job_status AS ENUM ('pending', 'processing', 'sent', 'failed', 'dead_letter');"
            )
            .await?;

        // 4. Recreate email_jobs table
        manager
            .create_table(
                Table::create() 
                    .table(EmailJobs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EmailJobs::Id)
                            .uuid()
                            .not_null()
                            .primary_key()
                            .extra("DEFAULT gen_random_uuid()"),
                    )
                    .col(
                        ColumnDef::new(EmailJobs::IdempotencyKey)
                            .string_len(255)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(EmailJobs::ToEmail)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(ColumnDef::new(EmailJobs::ToName).string_len(255))
                    .col(
                        ColumnDef::new(EmailJobs::Subject)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(ColumnDef::new(EmailJobs::HtmlBody).text().not_null())
                    .col(ColumnDef::new(EmailJobs::TextBody).text().not_null())
                    .col(
                        ColumnDef::new(EmailJobs::Status)
                            .custom(Alias::new("email_job_status"))
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(EmailJobs::RetryCount)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(EmailJobs::MaxRetries)
                            .integer()
                            .not_null()
                            .default(3),
                    )
                    .col(ColumnDef::new(EmailJobs::NextRetryAt).timestamp())
                    .col(ColumnDef::new(EmailJobs::LastError).text())
                    .col(
                        ColumnDef::new(EmailJobs::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(EmailJobs::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(EmailJobs::SentAt).timestamp())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    VerificationToken,
    TokenExpiresAt,
    ResetToken,
    ResetTokenExpiresAt,
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

#[derive(DeriveIden)]
enum EmailJobs {
    Table,
    Id,
    IdempotencyKey,
    ToEmail,
    ToName,
    Subject,
    HtmlBody,
    TextBody,
    Status,
    RetryCount,
    MaxRetries,
    NextRetryAt,
    LastError,
    CreatedAt,
    UpdatedAt,
    SentAt,
}
