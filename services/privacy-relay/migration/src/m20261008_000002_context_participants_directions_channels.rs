//! Context participants, allowed channels and explicit directions (Part D §27, §29).
//!
//! * `communication_context_participants`: authorization subjects with historical
//!   membership. Exactly one opaque subject reference — a CF Meet principal id or an
//!   opaque ContactReference id (a UUID: it can never carry a telephone number). A
//!   removed participant is never deleted; it keeps `removed_in_version` / `removed_at`
//!   as privacy-safe evidence and cannot be reactivated (set-once). At most one ACTIVE
//!   membership per subject per context, enforced by partial unique indexes, so
//!   concurrent duplicate additions cannot create two active rows.
//! * `communication_context_channels`: typed allowed channels (no free-form strings).
//!   MESSAGING may be declared for the future; nothing executes it.
//! * `communication_context_directions`: explicit directed edges between two distinct
//!   participants of the same context (and the same ownership), optionally channel
//!   specific; both participants must be active when the edge is created. Removed edges
//!   stay as history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE communication_context_participants (
    id                 UUID PRIMARY KEY,
    context_id         UUID        NOT NULL,
    tenant_id          UUID        NOT NULL,
    application_id     UUID        NOT NULL,
    environment_id     UUID        NOT NULL,
    subject_kind       TEXT        NOT NULL,
    principal_ref      UUID,
    contact_ref        UUID,
    added_in_version   BIGINT      NOT NULL,
    removed_in_version BIGINT,
    added_at           TIMESTAMPTZ NOT NULL,
    removed_at         TIMESTAMPTZ,
    CONSTRAINT fk_context_participants_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_context_participants_owner_id UNIQUE (tenant_id, application_id, environment_id, context_id, id),
    CONSTRAINT ck_context_participants_subject CHECK (
        (subject_kind = 'PRINCIPAL' AND principal_ref IS NOT NULL AND contact_ref IS NULL)
        OR (subject_kind = 'CONTACT_REFERENCE' AND contact_ref IS NOT NULL AND principal_ref IS NULL)),
    CONSTRAINT ck_context_participants_added CHECK (added_in_version >= 1),
    CONSTRAINT ck_context_participants_removed_pair CHECK ((removed_in_version IS NULL) = (removed_at IS NULL)),
    CONSTRAINT ck_context_participants_removed_after CHECK (
        removed_in_version IS NULL OR (removed_in_version > added_in_version AND removed_at >= added_at))
);
CREATE UNIQUE INDEX uq_context_participants_active_principal
    ON communication_context_participants (context_id, principal_ref)
    WHERE removed_at IS NULL AND principal_ref IS NOT NULL;
CREATE UNIQUE INDEX uq_context_participants_active_contact
    ON communication_context_participants (context_id, contact_ref)
    WHERE removed_at IS NULL AND contact_ref IS NOT NULL;
CREATE INDEX idx_context_participants_active ON communication_context_participants (context_id)
    WHERE removed_at IS NULL;
CREATE INDEX idx_context_participants_principal ON communication_context_participants (principal_ref)
    WHERE principal_ref IS NOT NULL;
CREATE INDEX idx_context_participants_contact ON communication_context_participants (contact_ref)
    WHERE contact_ref IS NOT NULL;

CREATE TRIGGER trg_context_participants_immutable
    BEFORE UPDATE ON communication_context_participants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'context_id', 'tenant_id', 'application_id', 'environment_id', 'subject_kind',
        'principal_ref', 'contact_ref', 'added_in_version', 'added_at');
CREATE TRIGGER trg_context_participants_set_once
    BEFORE UPDATE ON communication_context_participants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('removed_in_version', 'removed_at');
CREATE TRIGGER trg_context_participants_no_delete
    BEFORE DELETE ON communication_context_participants
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Allowed channels.
-- ---------------------------------------------------------------------------
CREATE TABLE communication_context_channels (
    context_id         UUID        NOT NULL,
    tenant_id          UUID        NOT NULL,
    application_id     UUID        NOT NULL,
    environment_id     UUID        NOT NULL,
    channel            TEXT        NOT NULL,
    enabled            BOOLEAN     NOT NULL,
    changed_in_version BIGINT      NOT NULL,
    created_at         TIMESTAMPTZ NOT NULL,
    updated_at         TIMESTAMPTZ NOT NULL,
    CONSTRAINT pk_context_channels PRIMARY KEY (context_id, channel),
    CONSTRAINT fk_context_channels_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_context_channels_channel CHECK (channel IN ('VOICE', 'MESSAGING')),
    CONSTRAINT ck_context_channels_version CHECK (changed_in_version >= 1),
    CONSTRAINT ck_context_channels_updated CHECK (updated_at >= created_at)
);
CREATE TRIGGER trg_context_channels_immutable
    BEFORE UPDATE ON communication_context_channels
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'context_id', 'tenant_id', 'application_id', 'environment_id', 'channel', 'created_at');
CREATE TRIGGER trg_context_channels_no_delete
    BEFORE DELETE ON communication_context_channels
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Explicit directed edges.
-- ---------------------------------------------------------------------------
CREATE TABLE communication_context_directions (
    id                  UUID PRIMARY KEY,
    context_id          UUID        NOT NULL,
    tenant_id           UUID        NOT NULL,
    application_id      UUID        NOT NULL,
    environment_id      UUID        NOT NULL,
    from_participant_id UUID        NOT NULL,
    to_participant_id   UUID        NOT NULL,
    channel             TEXT,
    added_in_version    BIGINT      NOT NULL,
    removed_in_version  BIGINT,
    created_at          TIMESTAMPTZ NOT NULL,
    removed_at          TIMESTAMPTZ,
    CONSTRAINT fk_context_directions_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_context_directions_from
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, from_participant_id)
        REFERENCES communication_context_participants (tenant_id, application_id, environment_id, context_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT fk_context_directions_to
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, to_participant_id)
        REFERENCES communication_context_participants (tenant_id, application_id, environment_id, context_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT uq_context_directions_owner_id UNIQUE (tenant_id, application_id, environment_id, context_id, id),
    CONSTRAINT ck_context_directions_distinct CHECK (from_participant_id <> to_participant_id),
    CONSTRAINT ck_context_directions_channel CHECK (channel IS NULL OR channel IN ('VOICE', 'MESSAGING')),
    CONSTRAINT ck_context_directions_added CHECK (added_in_version >= 1),
    CONSTRAINT ck_context_directions_removed_pair CHECK ((removed_in_version IS NULL) = (removed_at IS NULL)),
    CONSTRAINT ck_context_directions_removed_after CHECK (
        removed_in_version IS NULL OR (removed_in_version > added_in_version AND removed_at >= created_at))
);
CREATE UNIQUE INDEX uq_context_directions_active
    ON communication_context_directions (context_id, from_participant_id, to_participant_id, COALESCE(channel, '*'))
    WHERE removed_at IS NULL;
CREATE INDEX idx_context_directions_edge
    ON communication_context_directions (context_id, from_participant_id, to_participant_id);

-- Both endpoints of a new edge must be active participants.
CREATE FUNCTION privacy_relay_direction_participants_active() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM communication_context_participants p
               WHERE p.id IN (NEW.from_participant_id, NEW.to_participant_id) AND p.removed_at IS NOT NULL) THEN
        RAISE EXCEPTION 'privacy relay invariant violated: a direction requires two active participants'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_context_directions_active_participants
    BEFORE INSERT ON communication_context_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_direction_participants_active();
CREATE TRIGGER trg_context_directions_immutable
    BEFORE UPDATE ON communication_context_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'context_id', 'tenant_id', 'application_id', 'environment_id', 'from_participant_id',
        'to_participant_id', 'channel', 'added_in_version', 'created_at');
CREATE TRIGGER trg_context_directions_set_once
    BEFORE UPDATE ON communication_context_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_reassignment('removed_in_version', 'removed_at');
CREATE TRIGGER trg_context_directions_no_delete
    BEFORE DELETE ON communication_context_directions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS communication_context_directions;
DROP FUNCTION IF EXISTS privacy_relay_direction_participants_active();
DROP TABLE IF EXISTS communication_context_channels;
DROP TABLE IF EXISTS communication_context_participants;
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
