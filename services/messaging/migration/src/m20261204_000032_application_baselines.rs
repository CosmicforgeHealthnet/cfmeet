//! First-party application policy baselines (CF Meet chat remediation M1).
//!
//! * `messaging_application_baselines` — at most one row per application environment:
//!   the named, code-defined policy baseline (`conversations::policy::ApplicationBaseline`)
//!   that sits between the platform defaults and the environment's own application
//!   policy (`messaging_application_policies`). Effective policy = platform defaults ←
//!   baseline ← application policy, so an explicit application setting always wins and
//!   every environment without a row keeps the platform defaults unchanged.
//!
//! The only baseline is `CFMEET_FIRST_PARTY` (CF Meet's own personal application
//! environment, registered by the service at startup from
//! `CF_COMMUNICATIONS_PERSONAL_*`): attachments on, 25 MiB, IMAGE / DOCUMENT / FILE /
//! VOICE_NOTE / VIDEO. No other application is affected.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE messaging_application_baselines (
    environment_id  UUID         NOT NULL,
    tenant_id       UUID         NOT NULL,
    application_id  UUID         NOT NULL,
    baseline        VARCHAR(32)  NOT NULL,
    registered_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_messaging_application_baselines PRIMARY KEY (environment_id),
    CONSTRAINT ck_messaging_application_baselines_name CHECK (baseline IN ('CFMEET_FIRST_PARTY'))
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS messaging_application_baselines;
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
