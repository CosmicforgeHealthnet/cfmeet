use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE account_phone_bindings (
 user_id uuid PRIMARY KEY REFERENCES users(id), phone_e164 varchar(16) NOT NULL UNIQUE,
 verified_at timestamptz NOT NULL DEFAULT now(), discoverable boolean NOT NULL DEFAULT false,
 CHECK (phone_e164 ~ '^\+[1-9][0-9]{7,14}$')
);
CREATE TABLE account_phone_challenges (
 id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), session_id uuid,
 purpose varchar(8) NOT NULL CHECK (purpose IN ('BIND','SIGNIN')),
 phone_e164 varchar(16) NOT NULL, provider_id varchar(34) NOT NULL,
 attempts integer NOT NULL DEFAULT 0, created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL DEFAULT now() + interval '10 minutes', consumed_at timestamptz,
 CHECK (phone_e164 ~ '^\+[1-9][0-9]{7,14}$'), CHECK (attempts BETWEEN 0 AND 5),
 CHECK ((purpose='BIND' AND session_id IS NOT NULL) OR purpose='SIGNIN')
);
CREATE INDEX account_phone_challenge_rate ON account_phone_challenges(phone_e164,created_at);
CREATE INDEX account_phone_challenge_user_rate ON account_phone_challenges(user_id,created_at);
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS ck_account_sessions_auth_method;
ALTER TABLE account_sessions DROP CONSTRAINT IF EXISTS account_sessions_auth_method_check;
ALTER TABLE account_sessions ADD CONSTRAINT ck_account_sessions_auth_method
 CHECK (auth_method IN ('PASSWORD','EMAIL_OTP','PHONE_OTP','GOOGLE','LEGACY_REFRESH'));
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE account_phone_challenges; DROP TABLE account_phone_bindings;",
            )
            .await?;
        Ok(())
    }
}
