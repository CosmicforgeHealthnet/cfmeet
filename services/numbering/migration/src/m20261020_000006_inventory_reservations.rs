//! Workstream H: inventory search and reservation.
//!
//! * `number_reservations` records WHY a hold exists (`hold_basis`: a recorded ELIGIBLE
//!   decision, an explicitly permitted pre-eligibility hold, or a direct domain hold), the
//!   capabilities it was requested for, and what it was converted into (an assignment or a
//!   provisioned acquisition). The basis is immutable once recorded.
//! * Provider commands gain `RELEASE_HOLD`: giving a provider hold back when a reservation
//!   is cancelled or expires. It is lifecycle-affecting (one in flight per number) and
//!   names its reservation, like `RESERVE`.
//! * `number_inventory_refreshes` — one row per (provider, normalized query): when the
//!   provider was last asked, the outcome and when it may be asked again. Concurrent
//!   searches claim a refresh atomically, so one query never fans out to the provider.
//! * `number_search_windows` — fixed-window search counters per trusted environment
//!   (abuse-resistant enumeration limits shared by every Numbering instance).

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261020_000006_inventory_reservations"
    }
}

const UP: &str = r#"
-- ---------------------------------------------------------------------------
-- Reservation basis and conversion
-- ---------------------------------------------------------------------------
ALTER TABLE number_reservations
    ADD COLUMN hold_basis               VARCHAR(24) NOT NULL DEFAULT 'DIRECT',
    ADD COLUMN eligibility_decision_ref UUID,
    ADD COLUMN requested_capabilities   TEXT[]      NOT NULL DEFAULT '{}',
    ADD COLUMN converted_to             VARCHAR(16);
-- Reservations converted before H were all taken over by an assignment.
UPDATE number_reservations SET converted_to = 'ASSIGNMENT', revision = revision + 1 WHERE status = 'CONVERTED';
ALTER TABLE number_reservations
    ADD CONSTRAINT ck_number_reservations_basis CHECK (hold_basis IN
        ('DIRECT', 'ELIGIBILITY_DECISION', 'PRE_ELIGIBILITY_HOLD')
        AND (hold_basis = 'ELIGIBILITY_DECISION') = (eligibility_decision_ref IS NOT NULL)),
    ADD CONSTRAINT ck_number_reservations_capabilities CHECK (requested_capabilities <@ ARRAY[
        'VOICE_INBOUND', 'VOICE_OUTBOUND', 'SMS_INBOUND', 'SMS_OUTBOUND', 'MMS_RICH']::TEXT[]),
    ADD CONSTRAINT ck_number_reservations_converted CHECK ((status = 'CONVERTED') = (converted_to IS NOT NULL)
        AND (converted_to IS NULL OR converted_to IN ('ASSIGNMENT', 'ACQUISITION')));
-- A production reservation never rests on a pre-eligibility hold.
ALTER TABLE number_reservations
    ADD CONSTRAINT ck_number_reservations_production_basis CHECK (
        environment_kind <> 'PRODUCTION' OR hold_basis <> 'PRE_ELIGIBILITY_HOLD');
CREATE INDEX ix_number_reservations_release ON number_reservations (updated_at)
    WHERE provider_release_state IN ('PENDING', 'UNKNOWN');

CREATE OR REPLACE FUNCTION number_guard_reservation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    resource_state TEXT;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'reservations are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        SELECT state INTO resource_state FROM number_resources WHERE id = NEW.number_resource_id FOR UPDATE;
        IF resource_state NOT IN ('AVAILABLE', 'REASSIGNABLE') THEN
            RAISE EXCEPTION 'a % number resource cannot be reserved', resource_state USING ERRCODE = '23514';
        END IF;
        IF NEW.status <> 'PENDING' OR NEW.revision <> 1 OR NEW.converted_to IS NOT NULL THEN
            RAISE EXCEPTION 'a reservation starts PENDING at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id OR NEW.environment_kind IS DISTINCT FROM OLD.environment_kind
       OR NEW.service_account_id IS DISTINCT FROM OLD.service_account_id
       OR NEW.requested_for_principal_id IS DISTINCT FROM OLD.requested_for_principal_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id
       OR NEW.hold_basis IS DISTINCT FROM OLD.hold_basis
       OR NEW.eligibility_decision_ref IS DISTINCT FROM OLD.eligibility_decision_ref
       OR NEW.requested_capabilities IS DISTINCT FROM OLD.requested_capabilities THEN
        RAISE EXCEPTION 'the ownership and basis of a reservation are immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'reservation updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF OLD.status NOT IN ('PENDING', 'HELD', 'UNKNOWN') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % reservation is final', OLD.status USING ERRCODE = '42501';
    END IF;
    IF OLD.converted_to IS NOT NULL AND NEW.converted_to IS DISTINCT FROM OLD.converted_to THEN
        RAISE EXCEPTION 'a conversion is final' USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;

-- ---------------------------------------------------------------------------
-- RELEASE_HOLD provider commands
-- ---------------------------------------------------------------------------
ALTER TABLE number_provider_commands DROP CONSTRAINT ck_number_provider_commands_operation;
ALTER TABLE number_provider_commands ADD CONSTRAINT ck_number_provider_commands_operation CHECK (operation IN
    ('RESERVE', 'RELEASE_HOLD', 'PROVISION', 'CONFIGURE_VOICE', 'CONFIGURE_SMS', 'SUSPEND', 'RESTORE', 'RELEASE', 'PORT_IN'));
ALTER TABLE number_provider_commands DROP CONSTRAINT ck_number_provider_commands_reservation;
ALTER TABLE number_provider_commands ADD CONSTRAINT ck_number_provider_commands_reservation CHECK (
    (operation IN ('RESERVE', 'RELEASE_HOLD')) = (reservation_id IS NOT NULL));
DROP INDEX uq_number_provider_commands_in_flight;
CREATE UNIQUE INDEX uq_number_provider_commands_in_flight ON number_provider_commands (number_resource_id)
    WHERE status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')
      AND operation IN ('RESERVE', 'RELEASE_HOLD', 'PROVISION', 'SUSPEND', 'RESTORE', 'RELEASE', 'PORT_IN');
CREATE INDEX ix_number_provider_commands_reservation ON number_provider_commands (reservation_id)
    WHERE reservation_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Provider inventory refresh state (one row per provider and normalized query)
-- ---------------------------------------------------------------------------
CREATE TABLE number_inventory_refreshes (
    provider_id       VARCHAR(64)  NOT NULL,
    query_key         CHAR(64)     NOT NULL,
    country_code      CHAR(2)      NOT NULL,
    inventory_class   VARCHAR(16)  NOT NULL,
    claimed_until     TIMESTAMPTZ,
    last_attempt_at   TIMESTAMPTZ  NOT NULL,
    last_success_at   TIMESTAMPTZ,
    last_outcome      VARCHAR(16)  NOT NULL,
    offers            INTEGER      NOT NULL DEFAULT 0,
    next_refresh_at   TIMESTAMPTZ  NOT NULL,
    CONSTRAINT pk_number_inventory_refreshes PRIMARY KEY (provider_id, query_key),
    CONSTRAINT ck_number_inventory_refreshes_provider CHECK (provider_id ~ '^[a-z][a-z0-9-]{1,63}$'),
    CONSTRAINT ck_number_inventory_refreshes_key CHECK (query_key ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_number_inventory_refreshes_country CHECK (country_code ~ '^[A-Z]{2}$'),
    CONSTRAINT ck_number_inventory_refreshes_class CHECK (inventory_class IN ('REAL', 'SYNTHETIC')),
    CONSTRAINT ck_number_inventory_refreshes_outcome CHECK (last_outcome IN
        ('IN_PROGRESS', 'SUCCEEDED', 'UNAVAILABLE', 'UNSUPPORTED')),
    CONSTRAINT ck_number_inventory_refreshes_offers CHECK (offers >= 0)
);

-- ---------------------------------------------------------------------------
-- Search quota windows (per trusted application environment)
-- ---------------------------------------------------------------------------
CREATE TABLE number_search_windows (
    scope_key     VARCHAR(120) NOT NULL,
    window_start  TIMESTAMPTZ  NOT NULL,
    requests      INTEGER      NOT NULL,
    CONSTRAINT pk_number_search_windows PRIMARY KEY (scope_key, window_start),
    CONSTRAINT ck_number_search_windows_requests CHECK (requests >= 1)
);
"#;

const DOWN: &str = r#"
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM number_provider_commands WHERE operation = 'RELEASE_HOLD') THEN
        RAISE EXCEPTION 'hold releases are recorded; they cannot be represented before Workstream H';
    END IF;
    IF EXISTS (SELECT 1 FROM number_reservations WHERE converted_to = 'ACQUISITION' OR hold_basis <> 'DIRECT') THEN
        RAISE EXCEPTION 'reservations carry a Workstream H basis; they cannot be represented before Workstream H';
    END IF;
END $$;
DROP TABLE IF EXISTS number_search_windows;
DROP TABLE IF EXISTS number_inventory_refreshes;
DROP INDEX IF EXISTS ix_number_provider_commands_reservation;
DROP INDEX uq_number_provider_commands_in_flight;
CREATE UNIQUE INDEX uq_number_provider_commands_in_flight ON number_provider_commands (number_resource_id)
    WHERE status IN ('PENDING', 'DISPATCHING', 'UNKNOWN')
      AND operation IN ('RESERVE', 'PROVISION', 'SUSPEND', 'RESTORE', 'RELEASE', 'PORT_IN');
ALTER TABLE number_provider_commands DROP CONSTRAINT ck_number_provider_commands_reservation;
ALTER TABLE number_provider_commands ADD CONSTRAINT ck_number_provider_commands_reservation CHECK (
    (operation = 'RESERVE') = (reservation_id IS NOT NULL));
ALTER TABLE number_provider_commands DROP CONSTRAINT ck_number_provider_commands_operation;
ALTER TABLE number_provider_commands ADD CONSTRAINT ck_number_provider_commands_operation CHECK (operation IN
    ('RESERVE', 'PROVISION', 'CONFIGURE_VOICE', 'CONFIGURE_SMS', 'SUSPEND', 'RESTORE', 'RELEASE', 'PORT_IN'));

CREATE OR REPLACE FUNCTION number_guard_reservation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    resource_state TEXT;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'reservations are never deleted' USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'INSERT' THEN
        PERFORM number_guard_environment_inventory(NEW.number_resource_id, NEW.environment_kind);
        SELECT state INTO resource_state FROM number_resources WHERE id = NEW.number_resource_id FOR UPDATE;
        IF resource_state NOT IN ('AVAILABLE', 'REASSIGNABLE') THEN
            RAISE EXCEPTION 'a % number resource cannot be reserved', resource_state USING ERRCODE = '23514';
        END IF;
        IF NEW.status <> 'PENDING' OR NEW.revision <> 1 THEN
            RAISE EXCEPTION 'a reservation starts PENDING at revision 1' USING ERRCODE = '23514';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.id IS DISTINCT FROM OLD.id OR NEW.number_resource_id IS DISTINCT FROM OLD.number_resource_id
       OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id OR NEW.application_id IS DISTINCT FROM OLD.application_id
       OR NEW.environment_id IS DISTINCT FROM OLD.environment_id OR NEW.environment_kind IS DISTINCT FROM OLD.environment_kind
       OR NEW.service_account_id IS DISTINCT FROM OLD.service_account_id
       OR NEW.requested_for_principal_id IS DISTINCT FROM OLD.requested_for_principal_id
       OR NEW.created_at IS DISTINCT FROM OLD.created_at OR NEW.correlation_id IS DISTINCT FROM OLD.correlation_id THEN
        RAISE EXCEPTION 'the ownership of a reservation is immutable' USING ERRCODE = '42501';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
        RAISE EXCEPTION 'reservation updates advance the revision by exactly one' USING ERRCODE = '40001';
    END IF;
    IF OLD.status NOT IN ('PENDING', 'HELD', 'UNKNOWN') AND NEW.status IS DISTINCT FROM OLD.status THEN
        RAISE EXCEPTION 'a % reservation is final', OLD.status USING ERRCODE = '42501';
    END IF;
    RETURN NEW;
END $$;
DROP INDEX IF EXISTS ix_number_reservations_release;
ALTER TABLE number_reservations
    DROP CONSTRAINT ck_number_reservations_production_basis,
    DROP CONSTRAINT ck_number_reservations_converted,
    DROP CONSTRAINT ck_number_reservations_capabilities,
    DROP CONSTRAINT ck_number_reservations_basis,
    DROP COLUMN converted_to,
    DROP COLUMN requested_capabilities,
    DROP COLUMN eligibility_decision_ref,
    DROP COLUMN hold_basis;
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
