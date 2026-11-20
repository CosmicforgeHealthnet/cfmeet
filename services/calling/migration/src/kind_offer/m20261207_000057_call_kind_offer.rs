//! CF2 — a voice call may become a video call while it is still being offered.
//!
//! `calling_calls_guard_kind_change` (000049) let `calls.call_kind` move VOICE → VIDEO
//! only on an answered live Call (ANSWERED, CONNECTED, HELD). The caller may now switch
//! to video while the Call is DIALING or RINGING too, so the callee's offer shows (and is
//! answered as) a video call. Everything else is unchanged: VOICE → VIDEO once, never
//! back, never together with a state change, and never in a setup, TRANSFERRING, ENDING
//! or terminal state.
//!
//! Rollback-safe: the previous image never writes a kind change in DIALING / RINGING, so
//! the wider guard changes nothing for it; DOWN restores the 000049 guard.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE OR REPLACE FUNCTION calling_calls_guard_kind_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.call_kind IS DISTINCT FROM OLD.call_kind
       AND NOT (OLD.call_kind = 'VOICE' AND NEW.call_kind = 'VIDEO'
                AND OLD.current_state IN ('DIALING', 'RINGING', 'ANSWERED', 'CONNECTED', 'HELD')
                AND NEW.current_state = OLD.current_state) THEN
        RAISE EXCEPTION 'calling invariant violated: call % kind % -> % is not allowed in %',
            OLD.id, OLD.call_kind, NEW.call_kind, OLD.current_state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
"#;

const DOWN: &str = r#"
CREATE OR REPLACE FUNCTION calling_calls_guard_kind_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.call_kind IS DISTINCT FROM OLD.call_kind
       AND NOT (OLD.call_kind = 'VOICE' AND NEW.call_kind = 'VIDEO'
                AND OLD.current_state IN ('ANSWERED', 'CONNECTED', 'HELD')
                AND NEW.current_state = OLD.current_state) THEN
        RAISE EXCEPTION 'calling invariant violated: call % kind % -> % is not allowed in %',
            OLD.id, OLD.call_kind, NEW.call_kind, OLD.current_state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
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
