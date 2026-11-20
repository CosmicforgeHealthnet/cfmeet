//! S2 — account deletion reaches every service that holds the person's data.
//!
//! `account_privacy_jobs`: one durable job per deleted account and owner service
//! (`MESSAGING`: a PRINCIPAL_ERASURE deletion request, `COMMUNICATIONS`: the Hub's profile,
//! CF Meet ID, contacts, lists and saved messages, `CALLING`: the call history). Queued in
//! the same transaction that anonymizes the account, delivered and retried by the Meeting
//! Service's privacy worker (leased, `FOR UPDATE SKIP LOCKED`, exponential backoff), done
//! when the owner confirms. Unique per (account, target), so queuing is idempotent. Ids,
//! states and counts only: no personal data.
//!
//! Additive only, in a SEPARATE history (`meeting_account_privacy_migrations`), so the
//! previous image still starts after it is applied (it ignores the table). Down refuses
//! while a job is still open (an erasure is never silently dropped).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE IF NOT EXISTS account_privacy_jobs (
    id                UUID PRIMARY KEY,
    user_id           UUID NOT NULL,
    target            TEXT NOT NULL
        CONSTRAINT ck_account_privacy_jobs_target
        CHECK (target IN ('MESSAGING','COMMUNICATIONS','CALLING')),
    state             TEXT NOT NULL DEFAULT 'PENDING'
        CONSTRAINT ck_account_privacy_jobs_state
        CHECK (state IN ('PENDING','SUBMITTED','DONE')),
    remote_request_id UUID,
    attempts          INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_id          UUID,
    lease_until       TIMESTAMPTZ,
    last_error        VARCHAR(200),
    result            JSONB,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at      TIMESTAMPTZ,
    CONSTRAINT uq_account_privacy_jobs_user_target UNIQUE (user_id, target),
    CONSTRAINT ck_account_privacy_jobs_done CHECK ((state = 'DONE') = (completed_at IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_account_privacy_jobs_due
    ON account_privacy_jobs (next_attempt_at) WHERE state <> 'DONE';
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM account_privacy_jobs WHERE state <> 'DONE') THEN
        RAISE EXCEPTION 'account erasure jobs are still open; refusing to downgrade';
    END IF;
END $$;
DROP TABLE IF EXISTS account_privacy_jobs;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
