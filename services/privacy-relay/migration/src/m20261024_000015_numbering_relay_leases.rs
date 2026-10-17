//! Part IV Part P §§119–120: the relay-number authority moves to Numbering.
//!
//! `relay_number_leases` records, per relay-number assignment acquired while
//! `RELAY_NUMBER_AUTHORITY=NUMBERING` (or adopted at cutover), the Numbering lease that
//! authorizes it: ACTIVE while the assignment holds the number, RELEASE_PENDING once the
//! assignment ended (committed with the assignment's end), RELEASED when Numbering
//! confirmed the release. The assignment itself, its id, its generation and the session's
//! relay identity binding are unchanged: Privacy Relay still owns sessions and mappings.
//! Rows are never deleted (rollback to LOCAL keeps releasing what was leased).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE relay_number_leases (
    assignment_id      UUID        PRIMARY KEY REFERENCES relay_number_assignments (id) ON DELETE RESTRICT,
    relay_number_id    UUID        NOT NULL REFERENCES relay_number_inventory (id) ON DELETE RESTRICT,
    numbering_lease_id UUID        NOT NULL,
    generation         BIGINT      NOT NULL,
    status             TEXT        NOT NULL,
    acquired_at        TIMESTAMPTZ NOT NULL,
    release_requested_at TIMESTAMPTZ,
    released_at        TIMESTAMPTZ,
    release_attempts   INTEGER     NOT NULL DEFAULT 0,
    last_error         VARCHAR(64),
    CONSTRAINT uq_relay_number_leases_lease UNIQUE (numbering_lease_id),
    CONSTRAINT ck_relay_number_leases_status CHECK (status IN ('ACTIVE', 'RELEASE_PENDING', 'RELEASED')),
    CONSTRAINT ck_relay_number_leases_requested CHECK ((status = 'ACTIVE') = (release_requested_at IS NULL)),
    CONSTRAINT ck_relay_number_leases_released CHECK ((status = 'RELEASED') = (released_at IS NOT NULL)),
    CONSTRAINT ck_relay_number_leases_generation CHECK (generation >= 1),
    CONSTRAINT ck_relay_number_leases_error CHECK (last_error IS NULL OR last_error ~ '^[A-Z][A-Z0-9_]{0,63}$')
);
CREATE INDEX idx_relay_number_leases_pending ON relay_number_leases (release_requested_at)
    WHERE status = 'RELEASE_PENDING';

CREATE FUNCTION privacy_relay_guard_lease() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'privacy relay invariant violated: relay number leases are never deleted'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.assignment_id <> OLD.assignment_id OR NEW.relay_number_id <> OLD.relay_number_id
       OR NEW.numbering_lease_id <> OLD.numbering_lease_id OR NEW.generation <> OLD.generation
       OR NEW.acquired_at <> OLD.acquired_at THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a lease record identity is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NOT ((OLD.status, NEW.status) IN (('ACTIVE', 'ACTIVE'), ('ACTIVE', 'RELEASE_PENDING'),
            ('RELEASE_PENDING', 'RELEASE_PENDING'), ('RELEASE_PENDING', 'RELEASED'), ('RELEASED', 'RELEASED'))) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: lease % -> % is not allowed', OLD.status, NEW.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_relay_number_leases_guard
    BEFORE UPDATE OR DELETE ON relay_number_leases
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_guard_lease();
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM relay_number_leases WHERE status <> 'RELEASED') THEN
        RAISE EXCEPTION 'Numbering leases are still held; release them before rolling back';
    END IF;
END $$;
DROP TABLE IF EXISTS relay_number_leases;
DROP FUNCTION IF EXISTS privacy_relay_guard_lease();
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
