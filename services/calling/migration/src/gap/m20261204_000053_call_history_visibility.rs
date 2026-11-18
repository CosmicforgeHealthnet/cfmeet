//! Call history visibility (additive, backward compatible).
//!
//! * `call_participants.history_hidden_at` / `history_hidden_reason`: the participant's
//!   Call no longer appears in that principal's history. USER — the principal hid it
//!   (`DELETE /v1/calls/{id}/history?principalId=`); RETENTION — the retention worker
//!   purged it from every history once the Call ended longer ago than
//!   `CALLING_RETENTION_CALL_HISTORY_DAYS` (default 400). Set once, never cleared or
//!   reassigned (trigger). The canonical Call, its events and usage stay (they are
//!   retained evidence, ADR 0030).
//! * The retention vocabularies (holds, audit) gain the CALL_HISTORY data class.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const CLASSES_OLD: &str = "'CALL_RECORD', 'CALL_EVENT', 'CALL_USAGE', 'SPEND_EVIDENCE', \
    'WEBHOOK_EVENT_HISTORY', 'WEBHOOK_DELIVERY_HISTORY', 'CALLING_INCIDENT', \
    'MEDIA_PROVIDER_EVIDENCE', 'OPERATOR_AUDIT', 'ADMIN_AUDIT', 'RECORDING_ACCESS_AUDIT', \
    'RECORDING_ARTIFACT', 'VOICEMAIL_ARTIFACT', 'PRINCIPAL'";

fn up_sql() -> String {
    format!(
        r#"
ALTER TABLE call_participants
    ADD COLUMN history_hidden_at TIMESTAMPTZ,
    ADD COLUMN history_hidden_reason TEXT,
    ADD CONSTRAINT ck_call_participants_history_hidden CHECK (
        (history_hidden_at IS NULL) = (history_hidden_reason IS NULL)
        AND (history_hidden_reason IS NULL OR history_hidden_reason IN ('USER', 'RETENTION')));
CREATE TRIGGER trg_call_participants_history_hidden_once
    BEFORE UPDATE ON call_participants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_reassignment('history_hidden_at', 'history_hidden_reason');
ALTER TABLE calling_retention_holds
    DROP CONSTRAINT ck_calling_retention_holds_class,
    ADD CONSTRAINT ck_calling_retention_holds_class CHECK (data_class IN ('*', {CLASSES_OLD}, 'CALL_HISTORY'));
ALTER TABLE calling_retention_audit
    DROP CONSTRAINT ck_calling_retention_audit_class,
    ADD CONSTRAINT ck_calling_retention_audit_class CHECK (data_class IN ({CLASSES_OLD}, 'CALL_HISTORY'));
"#
    )
}

/// Restores the previous vocabularies NOT VALID: append-only audit rows (and holds)
/// written under CALL_HISTORY must not make the rollback fail.
fn down_sql() -> String {
    format!(
        r#"
ALTER TABLE calling_retention_audit
    DROP CONSTRAINT ck_calling_retention_audit_class,
    ADD CONSTRAINT ck_calling_retention_audit_class CHECK (data_class IN ({CLASSES_OLD})) NOT VALID;
ALTER TABLE calling_retention_holds
    DROP CONSTRAINT ck_calling_retention_holds_class,
    ADD CONSTRAINT ck_calling_retention_holds_class CHECK (data_class IN ('*', {CLASSES_OLD})) NOT VALID;
DROP TRIGGER IF EXISTS trg_call_participants_history_hidden_once ON call_participants;
ALTER TABLE call_participants
    DROP CONSTRAINT IF EXISTS ck_call_participants_history_hidden,
    DROP COLUMN IF EXISTS history_hidden_reason,
    DROP COLUMN IF EXISTS history_hidden_at;
"#
    )
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&up_sql())
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(&down_sql())
            .await?;
        Ok(())
    }
}
