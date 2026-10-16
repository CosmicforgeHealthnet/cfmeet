//! Privacy Relay integration evidence owned by Calling (Part D §29–§30).
//!
//! Calling never reads Privacy Relay's database; it records what it was TOLD, keyed by
//! the opaque cross-service references it already stores on the Call
//! (`calls.communication_context_ref`, `calls.privacy_relay_ref`):
//!
//! * `call_privacy_authorizations`: every Privacy Relay decision a protected call relied
//!   on (creation and each transfer introduction) — decision id, context / session
//!   versions, strategy and validity. Append-only. Used to decide which calls a later
//!   enforcement command applies to (calls authorized at or after the enforced version
//!   already reflect it).
//! * `call_privacy_enforcements`: the idempotency / dedupe record of enforcement commands
//!   received from Privacy Relay (one row per enforcement event id; a replay answers the
//!   recorded result without touching any call again). Restrictive only.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
CREATE TABLE call_privacy_authorizations (
    id                        UUID PRIMARY KEY,
    call_id                   UUID        NOT NULL,
    tenant_id                 UUID        NOT NULL,
    application_id            UUID        NOT NULL,
    environment_id            UUID        NOT NULL,
    purpose                   TEXT        NOT NULL,
    communication_context_ref UUID        NOT NULL,
    privacy_relay_ref         UUID        NOT NULL,
    context_version           BIGINT      NOT NULL,
    session_version           BIGINT      NOT NULL,
    decision_ref              UUID        NOT NULL,
    strategy                  VARCHAR(32) NOT NULL,
    policy_version            VARCHAR(32) NOT NULL,
    valid_until               TIMESTAMPTZ NOT NULL,
    target_principal_id       UUID,
    decided_at                TIMESTAMPTZ NOT NULL,
    created_at                TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_call_privacy_authorizations_call
        FOREIGN KEY (tenant_id, application_id, environment_id, call_id)
        REFERENCES calls (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT uq_call_privacy_authorizations_decision UNIQUE (decision_ref),
    CONSTRAINT ck_call_privacy_authorizations_purpose CHECK (purpose IN ('CALL_CREATE', 'CALL_TRANSFER')),
    CONSTRAINT ck_call_privacy_authorizations_versions CHECK (context_version >= 1 AND session_version >= 1),
    CONSTRAINT ck_call_privacy_authorizations_strategy
        CHECK (strategy IN ('APP_ONLY', 'APP_PREFERRED', 'PUBLIC_RELAY_ALLOWED', 'CALLBACK_RELAY_ALLOWED')),
    CONSTRAINT ck_call_privacy_authorizations_policy CHECK (policy_version ~ '^[A-Z][A-Z0-9_]{1,31}$'),
    CONSTRAINT ck_call_privacy_authorizations_transfer CHECK ((purpose = 'CALL_TRANSFER') = (target_principal_id IS NOT NULL))
);
CREATE INDEX idx_call_privacy_authorizations_call ON call_privacy_authorizations (call_id, decided_at);
CREATE INDEX idx_call_privacy_authorizations_context ON call_privacy_authorizations (communication_context_ref);
CREATE TRIGGER trg_call_privacy_authorizations_append_only
    BEFORE UPDATE OR DELETE ON call_privacy_authorizations
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();

CREATE TABLE call_privacy_enforcements (
    enforcement_event_id      UUID PRIMARY KEY,
    tenant_id                 UUID        NOT NULL,
    application_id            UUID        NOT NULL,
    environment_id            UUID        NOT NULL,
    communication_context_ref UUID        NOT NULL,
    context_version           BIGINT      NOT NULL,
    relay_session_refs        UUID[]      NOT NULL,
    session_version           BIGINT,
    affected_principal_refs   UUID[]      NOT NULL,
    reason                    TEXT        NOT NULL,
    terminate_active_calls    BOOLEAN     NOT NULL,
    requested_by_service      VARCHAR(64) NOT NULL,
    request_id                VARCHAR(36) NOT NULL,
    correlation_id            VARCHAR(37) NOT NULL,
    status                    TEXT        NOT NULL,
    matched_calls             INTEGER     NOT NULL,
    terminated_calls          INTEGER     NOT NULL,
    received_at               TIMESTAMPTZ NOT NULL,
    completed_at              TIMESTAMPTZ,
    CONSTRAINT ck_call_privacy_enforcements_version CHECK (context_version >= 1 AND (session_version IS NULL OR session_version >= 1)),
    CONSTRAINT ck_call_privacy_enforcements_sessions CHECK (cardinality(relay_session_refs) <= 16),
    CONSTRAINT ck_call_privacy_enforcements_principals CHECK (cardinality(affected_principal_refs) <= 256),
    CONSTRAINT ck_call_privacy_enforcements_reason CHECK (reason IN (
        'CONTEXT_REVOKED', 'CONTEXT_SUSPENDED', 'CONTEXT_COMPLETED', 'CONTEXT_EXPIRED',
        'PARTICIPANT_REMOVED', 'SESSION_REVOKED', 'SESSION_SUSPENDED', 'SESSION_EXPIRED')),
    CONSTRAINT ck_call_privacy_enforcements_status CHECK (status IN ('RECEIVED', 'APPLIED')),
    CONSTRAINT ck_call_privacy_enforcements_applied CHECK ((status = 'APPLIED') = (completed_at IS NOT NULL)),
    CONSTRAINT ck_call_privacy_enforcements_counts CHECK (matched_calls >= 0 AND terminated_calls >= 0 AND terminated_calls <= matched_calls)
);
CREATE INDEX idx_call_privacy_enforcements_context
    ON call_privacy_enforcements (communication_context_ref, context_version);
CREATE TRIGGER trg_call_privacy_enforcements_immutable
    BEFORE UPDATE ON call_privacy_enforcements
    FOR EACH ROW EXECUTE FUNCTION platform_forbid_column_change(
        'enforcement_event_id', 'tenant_id', 'application_id', 'environment_id',
        'communication_context_ref', 'context_version', 'relay_session_refs', 'session_version',
        'affected_principal_refs', 'reason', 'terminate_active_calls', 'requested_by_service',
        'request_id', 'correlation_id', 'received_at');
CREATE TRIGGER trg_call_privacy_enforcements_no_delete
    BEFORE DELETE ON call_privacy_enforcements
    FOR EACH ROW EXECUTE FUNCTION calling_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS call_privacy_enforcements;
DROP TABLE IF EXISTS call_privacy_authorizations;
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
