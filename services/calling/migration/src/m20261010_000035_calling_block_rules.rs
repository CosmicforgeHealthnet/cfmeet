//! Block / opt-out rules and repeated-refusal windows (Part H §70, §71, §83).
//!
//! `calling_block_rules`: a principal (or the whole application) refuses communication
//! with another principal or with an opaque Privacy Relay ContactReference. Never a
//! number: the blocked public party is the ContactReference id. One ACTIVE rule per
//! (blocker, blocked) pair; a release keeps the row (RELEASED, `released_at`) for
//! evidence. Rules are checked before any protected side effect and never terminate an
//! existing Call.
//!
//! `calling_abuse_windows`: UNLOGGED fixed-window counters of refusals (blocked attempts,
//! destination-policy denials) per environment and initiator principal, used only to
//! raise REPEATED_BLOCKED_ATTEMPTS / REPEATED_ROUTE_DENIALS incidents; disposable.
//!
//! `calling_incidents.category` gains the two categories and PRINCIPAL as a resource.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE calling_block_rules (
    id                        UUID        NOT NULL,
    tenant_id                 UUID        NOT NULL,
    application_id            UUID        NOT NULL,
    environment_id            UUID        NOT NULL,
    blocker_kind              TEXT        NOT NULL,
    blocker_principal_id      UUID,
    blocked_kind              TEXT        NOT NULL,
    blocked_principal_id      UUID,
    blocked_contact_reference UUID,
    reason_code               TEXT        NOT NULL,
    status                    TEXT        NOT NULL DEFAULT 'ACTIVE',
    created_by_credential_id  UUID        NOT NULL,
    created_at                TIMESTAMPTZ NOT NULL,
    released_at               TIMESTAMPTZ,
    version                   BIGINT      NOT NULL DEFAULT 1,
    CONSTRAINT pk_calling_block_rules PRIMARY KEY (id),
    CONSTRAINT fk_calling_block_rules_environment
        FOREIGN KEY (tenant_id, application_id, environment_id)
        REFERENCES platform_environments (tenant_id, application_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_block_rules_blocker
        FOREIGN KEY (tenant_id, application_id, environment_id, blocker_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_calling_block_rules_blocked
        FOREIGN KEY (tenant_id, application_id, environment_id, blocked_principal_id)
        REFERENCES platform_principals (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_calling_block_rules_blocker_kind CHECK (blocker_kind IN ('PRINCIPAL', 'APPLICATION')),
    CONSTRAINT ck_calling_block_rules_blocker
        CHECK ((blocker_kind = 'PRINCIPAL') = (blocker_principal_id IS NOT NULL)),
    CONSTRAINT ck_calling_block_rules_blocked_kind CHECK (blocked_kind IN ('PRINCIPAL', 'CONTACT_REFERENCE')),
    CONSTRAINT ck_calling_block_rules_blocked
        CHECK ((blocked_kind = 'PRINCIPAL') = (blocked_principal_id IS NOT NULL)
           AND (blocked_kind = 'CONTACT_REFERENCE') = (blocked_contact_reference IS NOT NULL)),
    CONSTRAINT ck_calling_block_rules_not_self
        CHECK (blocker_principal_id IS NULL OR blocked_principal_id IS NULL
               OR blocker_principal_id <> blocked_principal_id),
    CONSTRAINT ck_calling_block_rules_reason CHECK (reason_code IN ('UNWANTED', 'HARASSMENT', 'SPAM', 'OTHER')),
    CONSTRAINT ck_calling_block_rules_status CHECK (status IN ('ACTIVE', 'RELEASED')),
    CONSTRAINT ck_calling_block_rules_released CHECK ((status = 'RELEASED') = (released_at IS NOT NULL)),
    CONSTRAINT ck_calling_block_rules_version CHECK (version >= 1)
);
CREATE UNIQUE INDEX uq_calling_block_rules_active_pair ON calling_block_rules (
    environment_id, blocker_kind, COALESCE(blocker_principal_id, '00000000-0000-0000-0000-000000000000'),
    blocked_kind, COALESCE(blocked_principal_id, blocked_contact_reference))
    WHERE status = 'ACTIVE';
CREATE INDEX idx_calling_block_rules_blocked_principal
    ON calling_block_rules (environment_id, blocked_principal_id) WHERE status = 'ACTIVE';
CREATE INDEX idx_calling_block_rules_blocked_contact
    ON calling_block_rules (environment_id, blocked_contact_reference) WHERE status = 'ACTIVE';
CREATE INDEX idx_calling_block_rules_blocker
    ON calling_block_rules (environment_id, blocker_principal_id, created_at DESC);
CREATE TRIGGER trg_calling_block_rules_immutable
    BEFORE UPDATE ON calling_block_rules
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'id', 'tenant_id', 'application_id', 'environment_id', 'blocker_kind',
        'blocker_principal_id', 'blocked_kind', 'blocked_principal_id',
        'blocked_contact_reference', 'reason_code', 'created_by_credential_id', 'created_at');
CREATE TRIGGER trg_calling_block_rules_released_terminal
    BEFORE UPDATE ON calling_block_rules
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_leaving_terminal_status('RELEASED');
CREATE TRIGGER trg_calling_block_rules_no_delete
    BEFORE DELETE ON calling_block_rules
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

CREATE UNLOGGED TABLE calling_abuse_windows (
    environment_id UUID        NOT NULL,
    subject_kind   VARCHAR(32) NOT NULL,
    subject_id     UUID        NOT NULL,
    window_start   TIMESTAMPTZ NOT NULL,
    hits           BIGINT      NOT NULL,
    CONSTRAINT pk_calling_abuse_windows PRIMARY KEY (environment_id, subject_kind, subject_id, window_start),
    CONSTRAINT ck_calling_abuse_windows_kind
        CHECK (subject_kind IN ('BLOCKED_ATTEMPT', 'ROUTE_DENIAL')),
    CONSTRAINT ck_calling_abuse_windows_hits CHECK (hits > 0)
);

ALTER TABLE calling_incidents
    DROP CONSTRAINT ck_calling_incidents_category,
    ADD CONSTRAINT ck_calling_incidents_category CHECK (category IN (
        'STUCK_QUEUE_ENTRY', 'QUEUE_ASSIGNMENT_CONFLICT', 'IVR_ROUTING_CORRUPTION',
        'VOICEMAIL_FINALIZATION_FAILURE', 'RECORDING_ARTIFACT_FAILURE', 'FORWARDING_LOOP_REPEATED',
        'REPEATED_BLOCKED_ATTEMPTS', 'REPEATED_ROUTE_DENIALS', 'ABUSE_REPORTS_BURST')),
    DROP CONSTRAINT ck_calling_incidents_resource_type,
    ADD CONSTRAINT ck_calling_incidents_resource_type CHECK (resource_type IS NULL OR resource_type IN (
        'QUEUE_ENTRY', 'IVR_SESSION', 'VOICEMAIL_MESSAGE', 'CALL_RECORDING', 'DESTINATION', 'PRINCIPAL'));
"#;

const DOWN: &str = r#"
DELETE FROM calling_incidents WHERE category IN (
    'REPEATED_BLOCKED_ATTEMPTS', 'REPEATED_ROUTE_DENIALS', 'ABUSE_REPORTS_BURST')
    OR resource_type = 'PRINCIPAL';
ALTER TABLE calling_incidents
    DROP CONSTRAINT ck_calling_incidents_category,
    ADD CONSTRAINT ck_calling_incidents_category CHECK (category IN (
        'STUCK_QUEUE_ENTRY', 'QUEUE_ASSIGNMENT_CONFLICT', 'IVR_ROUTING_CORRUPTION',
        'VOICEMAIL_FINALIZATION_FAILURE', 'RECORDING_ARTIFACT_FAILURE', 'FORWARDING_LOOP_REPEATED')),
    DROP CONSTRAINT ck_calling_incidents_resource_type,
    ADD CONSTRAINT ck_calling_incidents_resource_type CHECK (resource_type IS NULL OR resource_type IN (
        'QUEUE_ENTRY', 'IVR_SESSION', 'VOICEMAIL_MESSAGE', 'CALL_RECORDING', 'DESTINATION'));
DROP TABLE IF EXISTS calling_abuse_windows;
DROP TABLE IF EXISTS calling_block_rules;
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
