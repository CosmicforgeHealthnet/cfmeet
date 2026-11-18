//! Attended (consultative) transfer (gap closure C14 / CAL-019, additive).
//!
//! `call_consultations` links an ORIGINAL Call to the CONSULTATION Call its transferring
//! participant placed to the transfer target while the original was held:
//!
//! ```text
//! CONSULTING ──complete──▶ COMPLETING ──target joined the original──▶ COMPLETED
//!     │   ▲                     │
//!     │   └──transfer failed────┘
//!     └──cancel──▶ CANCELLED
//! ```
//!
//! The completion itself is the canonical Part C transfer of the ORIGINAL Call to the
//! target (`call_transfers`); this row only records the consultation, which Call it
//! consulted on and which transfer completed it. Both Calls stay ordinary canonical Calls.
//! At most one active (CONSULTING / COMPLETING) consultation per original Call; a
//! Call is the consultation of at most one original. Final states never change.
//!
//! Rollback note: the previous image never reads the table (consultations in flight
//! simply stop being completable through it; both Calls stay ordinary Calls). DOWN
//! refuses while active consultations exist rather than dropping in-flight evidence.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE call_consultations (
    id                          UUID        NOT NULL,
    tenant_id                   UUID        NOT NULL,
    application_id              UUID        NOT NULL,
    environment_id              UUID        NOT NULL,
    call_id                     UUID        NOT NULL,
    consultation_call_id        UUID        NOT NULL,
    transferring_participant_id UUID        NOT NULL,
    target_principal_id         UUID        NOT NULL,
    state                       TEXT        NOT NULL,
    held_original               BOOLEAN     NOT NULL,
    transfer_id                 UUID,
    completion_attempts         INTEGER     NOT NULL,
    version                     BIGINT      NOT NULL,
    created_at                  TIMESTAMPTZ NOT NULL,
    updated_at                  TIMESTAMPTZ NOT NULL,
    ended_at                    TIMESTAMPTZ,
    CONSTRAINT pk_call_consultations PRIMARY KEY (id),
    CONSTRAINT uq_call_consultations_consultation_call UNIQUE (consultation_call_id),
    CONSTRAINT fk_call_consultations_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_consultations_consultation_call
        FOREIGN KEY (tenant_id, application_id, environment_id, consultation_call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_call_consultations_transferring_participant
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, transferring_participant_id)
        REFERENCES call_participants (tenant_id, application_id, environment_id, call_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_consultations_target_principal
        FOREIGN KEY (tenant_id, application_id, environment_id, target_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_call_consultations_transfer
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id, transfer_id)
        REFERENCES call_transfers (tenant_id, application_id, environment_id, call_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT ck_call_consultations_distinct_calls CHECK (call_id <> consultation_call_id),
    CONSTRAINT ck_call_consultations_state
        CHECK (state IN ('CONSULTING', 'COMPLETING', 'COMPLETED', 'CANCELLED')),
    CONSTRAINT ck_call_consultations_ended
        CHECK ((state IN ('COMPLETED', 'CANCELLED')) = (ended_at IS NOT NULL)),
    CONSTRAINT ck_call_consultations_transfer
        CHECK (state NOT IN ('COMPLETING', 'COMPLETED') OR transfer_id IS NOT NULL),
    CONSTRAINT ck_call_consultations_attempts CHECK (completion_attempts >= 0),
    CONSTRAINT ck_call_consultations_version CHECK (version >= 1),
    CONSTRAINT ck_call_consultations_timestamps
        CHECK (updated_at >= created_at AND (ended_at IS NULL OR ended_at >= created_at))
);
-- One active consultation per original Call.
CREATE UNIQUE INDEX uq_call_consultations_active
    ON call_consultations (call_id) WHERE state IN ('CONSULTING', 'COMPLETING');
CREATE INDEX idx_call_consultations_scope
    ON call_consultations (tenant_id, application_id, environment_id, call_id, created_at);
CREATE TRIGGER trg_call_consultations_immutable
    BEFORE UPDATE ON call_consultations
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'call_id',
        'consultation_call_id', 'transferring_participant_id', 'target_principal_id',
        'held_original', 'created_at');
-- COMPLETED / CANCELLED are final.
CREATE FUNCTION calling_call_consultations_final() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state IN ('COMPLETED', 'CANCELLED') THEN
        RAISE EXCEPTION 'calling invariant violated: consultation % is % (final)', OLD.id, OLD.state
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_call_consultations_final
    BEFORE UPDATE ON call_consultations
    FOR EACH ROW EXECUTE FUNCTION calling_call_consultations_final();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM call_consultations WHERE state IN ('CONSULTING', 'COMPLETING')) THEN
        RAISE EXCEPTION 'call_consultations has active consultations; finish or cancel them before rolling back';
    END IF;
END;
$$;
DROP TABLE call_consultations;
DROP FUNCTION calling_call_consultations_final();
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
