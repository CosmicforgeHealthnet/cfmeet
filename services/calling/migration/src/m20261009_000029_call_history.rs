//! Call history continuity (Part F §62, §130–§137; ADR 0025).
//!
//! History is DERIVED from the canonical Call evidence (participants, ring offers,
//! sessions and their connection intervals, the Call's terminal state, the enterprise
//! context); nothing here stores a "missed" flag, and no push, realtime signal or client
//! acknowledgement is an input.
//!
//! * `call_participants.call_created_at` — an immutable copy of the Call's `created_at`,
//!   stamped by trigger on insert (never by a client), so a principal's history is ONE
//!   keyset index range in the same order as `GET /v1/calls` (`created_at DESC, id DESC`).
//! * `idx_call_participants_history` — (scope, principal, call_created_at DESC, call_id
//!   DESC): participant-scoped history never scans the whole table.
//! * `idx_enterprise_routing_steps_destination` — Calls routed through one extension,
//!   ring group, queue, IVR flow or mailbox.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
ALTER TABLE call_participants ADD COLUMN call_created_at TIMESTAMPTZ;
UPDATE call_participants p SET call_created_at = c.created_at FROM calls c WHERE c.id = p.call_id;
ALTER TABLE call_participants ALTER COLUMN call_created_at SET NOT NULL;

CREATE FUNCTION call_participants_stamp_call_created_at() RETURNS trigger AS $$
BEGIN
    SELECT c.created_at INTO NEW.call_created_at FROM calls c WHERE c.id = NEW.call_id;
    IF NEW.call_created_at IS NULL THEN
        RAISE EXCEPTION 'call_participants: unknown call' USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END
$$ LANGUAGE plpgsql;
CREATE TRIGGER trg_call_participants_call_created_at
    BEFORE INSERT ON call_participants
    FOR EACH ROW EXECUTE FUNCTION call_participants_stamp_call_created_at();
CREATE TRIGGER trg_call_participants_history_immutable
    BEFORE UPDATE ON call_participants
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change('call_created_at');

CREATE INDEX idx_call_participants_history ON call_participants
    (tenant_id, application_id, environment_id, principal_id, call_created_at DESC, call_id DESC);
CREATE INDEX idx_enterprise_routing_steps_destination ON enterprise_routing_steps
    (tenant_id, application_id, environment_id, destination_type, destination_id)
    WHERE destination_id IS NOT NULL;
"#;

const DOWN: &str = r#"
DROP INDEX IF EXISTS idx_enterprise_routing_steps_destination;
DROP INDEX IF EXISTS idx_call_participants_history;
DROP TRIGGER IF EXISTS trg_call_participants_history_immutable ON call_participants;
DROP TRIGGER IF EXISTS trg_call_participants_call_created_at ON call_participants;
DROP FUNCTION IF EXISTS call_participants_stamp_call_created_at();
ALTER TABLE call_participants DROP COLUMN IF EXISTS call_created_at;
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
