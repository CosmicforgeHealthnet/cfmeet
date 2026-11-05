//! Communications Hub meeting facade (Sections 9–23, MT-1): idempotency of instant
//! meetings the Hub starts for a delegated account. Ids only — no title, passcode,
//! invitation, participant or media material. The meeting itself stays canonical in
//! `meeting_spaces` / `conference_sessions`.
use sea_orm_migration::prelude::*;
#[derive(DeriveMigrationName)]
pub struct Migration;
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, m: &SchemaManager) -> Result<(), DbErr> {
        m.get_connection()
            .execute_unprepared(
                r#"
CREATE TABLE communication_instant_meetings (
 user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 idempotency_key UUID NOT NULL,
 meeting_space_id UUID NOT NULL,
 conference_id UUID NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY (user_id, idempotency_key)
);
"#,
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(
            "Communications meeting facade migration is additive and forward-only".into(),
        ))
    }
}
