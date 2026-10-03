use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create email job status enum
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TYPE email_job_status AS ENUM ('pending', 'processing', 'sent', 'failed', 'dead_letter');"
            )
            .await?;

        // Create email_jobs table
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
                    // Idempotency key prevents duplicate sends
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

        // Index for fetching pending jobs efficiently
        manager
            .create_index(
                Index::create()
                    .name("idx_email_jobs_status_next_retry")
                    .table(EmailJobs::Table)
                    .col(EmailJobs::Status)
                    .col(EmailJobs::NextRetryAt)
                    .to_owned(),
            )
            .await?;

        // Index for idempotency lookups
        manager
            .create_index(
                Index::create()
                    .name("idx_email_jobs_idempotency_key")
                    .table(EmailJobs::Table)
                    .col(EmailJobs::IdempotencyKey)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EmailJobs::Table).to_owned())
            .await?;

        manager
            .get_connection()
            .execute_unprepared("DROP TYPE IF EXISTS email_job_status;")
            .await?;

        Ok(())
    }
}

#[derive(DeriveIden)]
pub enum EmailJobs {
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
