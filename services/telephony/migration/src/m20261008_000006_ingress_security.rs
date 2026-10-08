//! Ingress security windows (Part E §49, §169, §176): short fixed-window counters of
//! provider-callback authentication failures and SIP / provider ingress attempts, used
//! for bounded rate limiting and to raise CALLBACK_AUTH_FAILURES incidents. UNLOGGED and
//! disposable (a crash resets the current window); pruned continuously. Never a number,
//! address, token or payload: the subject is a provider id or a peer configuration id.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE UNLOGGED TABLE telephony_ingress_windows (
    subject_kind VARCHAR(32) NOT NULL,
    subject_id   VARCHAR(64) NOT NULL,
    window_start TIMESTAMPTZ NOT NULL,
    hits         BIGINT      NOT NULL,
    CONSTRAINT pk_telephony_ingress_windows PRIMARY KEY (subject_kind, subject_id, window_start),
    CONSTRAINT ck_telephony_ingress_windows_kind
        CHECK (subject_kind IN ('CALLBACK_AUTH_FAILURE', 'CALLBACK_ACCEPTED', 'SIP_INGRESS')),
    CONSTRAINT ck_telephony_ingress_windows_subject CHECK (subject_id ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    CONSTRAINT ck_telephony_ingress_windows_hits CHECK (hits > 0)
);
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS telephony_ingress_windows;
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
