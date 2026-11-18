//! Gap audit A17: emails and usernames are unique regardless of case.
//!
//! The application already resolves both case-insensitively (`find_user_by_email`,
//! `username_taken`). These unique expression indexes make the database enforce it. They
//! are created only when no case-only duplicates exist: if a legacy deployment holds any,
//! the migration still succeeds, leaves that index absent and reports it (NOTICE), and
//! `GET /api/v2/admin/identity-duplicates` lists the rows to reconcile; re-running the
//! check later (`SELECT ensure_case_insensitive_identity()`) creates what is now possible.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE OR REPLACE FUNCTION ensure_case_insensitive_identity() RETURNS TEXT
LANGUAGE plpgsql AS $$
DECLARE
    result TEXT := '';
BEGIN
    IF EXISTS (SELECT 1 FROM users GROUP BY LOWER(email) HAVING count(*) > 1) THEN
        RAISE NOTICE 'users: case-only duplicate emails exist; uq_users_email_lower deferred';
        result := result || 'email:DEFERRED ';
    ELSE
        CREATE UNIQUE INDEX IF NOT EXISTS uq_users_email_lower ON users (LOWER(email));
        result := result || 'email:ENFORCED ';
    END IF;
    IF EXISTS (SELECT 1 FROM users GROUP BY LOWER(username) HAVING count(*) > 1) THEN
        RAISE NOTICE 'users: case-only duplicate usernames exist; uq_users_username_lower deferred';
        result := result || 'username:DEFERRED';
    ELSE
        CREATE UNIQUE INDEX IF NOT EXISTS uq_users_username_lower ON users (LOWER(username));
        result := result || 'username:ENFORCED';
    END IF;
    RETURN result;
END;
$$;
SELECT ensure_case_insensitive_identity();
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS uq_users_username_lower;
DROP INDEX IF EXISTS uq_users_email_lower;
DROP FUNCTION IF EXISTS ensure_case_insensitive_identity();
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
