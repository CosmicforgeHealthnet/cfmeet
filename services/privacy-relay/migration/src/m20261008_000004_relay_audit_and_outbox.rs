//! Authorization decisions, immutable audit evidence and the transactional outbox
//! (Part D §29–§30).
//!
//! * `relay_authorization_decisions`: every communication authorization decision —
//!   AUTHORIZED or DENIED with its normalized reason — with the context / session
//!   versions it was evaluated against and safe subject references only (principal ids,
//!   opaque ContactReference ids). Append-only.
//! * `relay_audit_events`: append-only evidence of every authority change and of
//!   enforcement delivery, gap-free per context (`sequence`). The database rejects UPDATE
//!   and DELETE. (Communication decisions are recorded once, in
//!   `relay_authorization_decisions`, the queryable decision audit.)
//! * `privacy_relay_outbox`: one row per authority-change audit event, in the canonical
//!   platform event envelope, written in the same transaction as the state change.
//!
//! None of these carry telephone numbers, provider identifiers, credentials or tokens
//! (bounded, typed columns; JSON bodies are checked for credential- / contact-shaped
//! keys).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = r#"
-- JSON documents that may never carry secret- or contact-shaped members.
CREATE FUNCTION privacy_relay_safe_document(doc jsonb) RETURNS boolean
LANGUAGE sql IMMUTABLE AS $$
    SELECT doc IS NOT NULL
       AND jsonb_typeof(doc) = 'object'
       AND octet_length(doc::text) <= 65536
       AND doc::text !~* '"[a-z_]*(password|secret|token|credential|api_?key|phone|msisdn|e164|sip_?uri|telephone)[a-z_]*"\s*:'
       AND doc::text !~* 'cfmk_'
$$;

CREATE TABLE relay_authorization_decisions (
    id                    UUID PRIMARY KEY,
    tenant_id             UUID        NOT NULL,
    application_id        UUID        NOT NULL,
    environment_id        UUID        NOT NULL,
    context_id            UUID        NOT NULL,
    session_id            UUID        NOT NULL,
    context_version       BIGINT      NOT NULL,
    session_version       BIGINT      NOT NULL,
    source_subject_kind   TEXT        NOT NULL,
    source_ref            UUID        NOT NULL,
    target_principal_refs UUID[]      NOT NULL,
    target_contact_refs   UUID[]      NOT NULL,
    channel               TEXT        NOT NULL,
    route                 TEXT        NOT NULL,
    purpose               TEXT        NOT NULL,
    decision              TEXT        NOT NULL,
    reason                TEXT        NOT NULL,
    strategy              TEXT        NOT NULL,
    policy_version        VARCHAR(32) NOT NULL,
    valid_until           TIMESTAMPTZ,
    requester_service     VARCHAR(64) NOT NULL,
    request_id            VARCHAR(36) NOT NULL,
    correlation_id        VARCHAR(37) NOT NULL,
    decided_at            TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_relay_decisions_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT ck_relay_decisions_source_kind CHECK (source_subject_kind IN ('PRINCIPAL', 'CONTACT_REFERENCE')),
    CONSTRAINT ck_relay_decisions_targets CHECK (
        cardinality(target_principal_refs) + cardinality(target_contact_refs) BETWEEN 1 AND 64),
    CONSTRAINT ck_relay_decisions_channel CHECK (channel IN ('VOICE', 'MESSAGING')),
    CONSTRAINT ck_relay_decisions_route CHECK (route IN ('APPLICATION', 'PUBLIC_RELAY', 'CALLBACK_RELAY')),
    CONSTRAINT ck_relay_decisions_purpose CHECK (purpose IN ('CALL_CREATE', 'CALL_TRANSFER', 'CHECK')),
    CONSTRAINT ck_relay_decisions_decision CHECK (decision IN ('AUTHORIZED', 'DENIED')),
    CONSTRAINT ck_relay_decisions_reason CHECK (reason IN (
        'AUTHORIZED', 'FEATURE_DISABLED', 'CONTEXT_NOT_ACTIVE', 'CONTEXT_NOT_STARTED', 'CONTEXT_EXPIRED',
        'CONTEXT_REVOKED', 'CONTEXT_SUSPENDED', 'SESSION_NOT_ACTIVE', 'SESSION_NOT_STARTED', 'SESSION_EXPIRED',
        'PARTICIPANT_NOT_AUTHORIZED', 'DIRECTION_NOT_ALLOWED', 'CHANNEL_NOT_ALLOWED', 'STRATEGY_NOT_ALLOWED',
        'GRACE_POLICY_DENIED', 'CAPABILITY_UNAVAILABLE')),
    CONSTRAINT ck_relay_decisions_consistent CHECK (
        (decision = 'AUTHORIZED') = (reason = 'AUTHORIZED')
        AND (decision = 'AUTHORIZED') = (valid_until IS NOT NULL)),
    CONSTRAINT ck_relay_decisions_strategy
        CHECK (strategy IN ('APP_ONLY', 'APP_PREFERRED', 'PUBLIC_RELAY_ALLOWED', 'CALLBACK_RELAY_ALLOWED')),
    CONSTRAINT ck_relay_decisions_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$')
);
CREATE INDEX idx_relay_decisions_context ON relay_authorization_decisions (context_id, decided_at DESC);
CREATE INDEX idx_relay_decisions_session ON relay_authorization_decisions (session_id, decided_at DESC);
CREATE INDEX idx_relay_decisions_scope ON relay_authorization_decisions (tenant_id, application_id, environment_id, decided_at DESC);
CREATE TRIGGER trg_relay_decisions_append_only
    BEFORE UPDATE OR DELETE ON relay_authorization_decisions
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Immutable audit evidence.
-- ---------------------------------------------------------------------------
CREATE TABLE relay_audit_events (
    id                       UUID PRIMARY KEY,
    tenant_id                UUID        NOT NULL,
    application_id           UUID        NOT NULL,
    environment_id           UUID        NOT NULL,
    context_id               UUID        NOT NULL,
    session_id               UUID,
    sequence                 BIGINT      NOT NULL,
    event_type               TEXT        NOT NULL,
    context_version          BIGINT      NOT NULL,
    session_version          BIGINT,
    actor_kind               TEXT        NOT NULL,
    actor_service_account_id UUID        NOT NULL,
    actor_service            VARCHAR(64),
    decision_id              UUID,
    enforcement_job_id       UUID,
    request_id               VARCHAR(36) NOT NULL,
    correlation_id           VARCHAR(37) NOT NULL,
    data                     JSONB       NOT NULL,
    occurred_at              TIMESTAMPTZ NOT NULL,
    CONSTRAINT fk_relay_audit_context
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id)
        REFERENCES communication_contexts (tenant_id, application_id, environment_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_relay_audit_session
        FOREIGN KEY (tenant_id, application_id, environment_id, context_id, session_id)
        REFERENCES privacy_relay_sessions (tenant_id, application_id, environment_id, context_id, id) ON DELETE RESTRICT,
    CONSTRAINT fk_relay_audit_decision FOREIGN KEY (decision_id)
        REFERENCES relay_authorization_decisions (id) ON DELETE RESTRICT,
    CONSTRAINT uq_relay_audit_sequence UNIQUE (context_id, sequence),
    CONSTRAINT ck_relay_audit_sequence CHECK (sequence >= 1),
    CONSTRAINT ck_relay_audit_event_type CHECK (event_type IN (
        'CONTEXT_CREATED', 'CONTEXT_UPDATED', 'CONTEXT_PARTICIPANTS_CHANGED', 'CONTEXT_SUSPENDED',
        'CONTEXT_RESUMED', 'CONTEXT_REVOKED', 'CONTEXT_COMPLETED', 'CONTEXT_EXPIRED',
        'RELAY_SESSION_CREATED', 'RELAY_SESSION_UPDATED', 'RELAY_SESSION_SUSPENDED', 'RELAY_SESSION_RESUMED',
        'RELAY_SESSION_REVOKED', 'RELAY_SESSION_COMPLETED', 'RELAY_SESSION_EXPIRED',
        'ENFORCEMENT_REQUESTED', 'ENFORCEMENT_APPLIED', 'ENFORCEMENT_FAILED')),
    CONSTRAINT ck_relay_audit_versions CHECK (context_version >= 1 AND (session_version IS NULL OR session_version >= 1)),
    CONSTRAINT ck_relay_audit_session_version CHECK ((session_id IS NULL) = (session_version IS NULL)),
    CONSTRAINT ck_relay_audit_actor_kind CHECK (actor_kind IN ('APPLICATION', 'SYSTEM')),
    CONSTRAINT ck_relay_audit_ids CHECK (
        request_id ~ '^req_[0-9a-f]{32}$' AND correlation_id ~ '^corr_[0-9a-f]{32}$'),
    CONSTRAINT ck_relay_audit_data CHECK (privacy_relay_safe_document(data) AND octet_length(data::text) <= 16384)
);
CREATE INDEX idx_relay_audit_context ON relay_audit_events (context_id, sequence);
CREATE INDEX idx_relay_audit_session ON relay_audit_events (session_id, occurred_at) WHERE session_id IS NOT NULL;
CREATE TRIGGER trg_relay_audit_append_only
    BEFORE UPDATE OR DELETE ON relay_audit_events
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();

-- ---------------------------------------------------------------------------
-- Transactional outbox (canonical platform event envelope).
-- ---------------------------------------------------------------------------
CREATE TABLE privacy_relay_outbox (
    id                    UUID PRIMARY KEY,
    event_id              VARCHAR(36)  NOT NULL,
    source_audit_event_id UUID         NOT NULL,
    aggregate_type        TEXT         NOT NULL,
    aggregate_id          UUID         NOT NULL,
    tenant_id             UUID         NOT NULL,
    application_id        UUID         NOT NULL,
    environment_id        UUID         NOT NULL,
    event_type            VARCHAR(128) NOT NULL,
    schema_version        INTEGER      NOT NULL,
    envelope              JSONB        NOT NULL,
    status                TEXT         NOT NULL,
    attempt_count         INTEGER      NOT NULL,
    next_attempt_at       TIMESTAMPTZ  NOT NULL,
    published_at          TIMESTAMPTZ,
    created_at            TIMESTAMPTZ  NOT NULL,
    updated_at            TIMESTAMPTZ  NOT NULL,
    CONSTRAINT fk_relay_outbox_source FOREIGN KEY (source_audit_event_id)
        REFERENCES relay_audit_events (id) ON DELETE RESTRICT,
    CONSTRAINT uq_relay_outbox_event_id UNIQUE (event_id),
    CONSTRAINT uq_relay_outbox_source UNIQUE (source_audit_event_id),
    CONSTRAINT ck_relay_outbox_event_id CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    CONSTRAINT ck_relay_outbox_aggregate_type CHECK (aggregate_type IN ('COMMUNICATION_CONTEXT', 'RELAY_SESSION')),
    CONSTRAINT ck_relay_outbox_event_type CHECK (event_type ~ '^privacy-relay(\.[a-z][a-z0-9-]*)+$'),
    CONSTRAINT ck_relay_outbox_schema_version CHECK (schema_version >= 1),
    CONSTRAINT ck_relay_outbox_status CHECK (status IN ('PENDING', 'PUBLISHED')),
    CONSTRAINT ck_relay_outbox_attempts CHECK (attempt_count >= 0),
    CONSTRAINT ck_relay_outbox_published CHECK ((status = 'PUBLISHED') = (published_at IS NOT NULL)),
    CONSTRAINT ck_relay_outbox_envelope CHECK (privacy_relay_safe_document(envelope))
);
CREATE INDEX idx_relay_outbox_pending ON privacy_relay_outbox (next_attempt_at) WHERE status = 'PENDING';
CREATE TRIGGER trg_relay_outbox_immutable
    BEFORE UPDATE ON privacy_relay_outbox
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_column_change(
        'id', 'event_id', 'source_audit_event_id', 'aggregate_type', 'aggregate_id', 'tenant_id',
        'application_id', 'environment_id', 'event_type', 'schema_version', 'envelope', 'created_at');
CREATE TRIGGER trg_relay_outbox_no_delete
    BEFORE DELETE ON privacy_relay_outbox
    FOR EACH ROW EXECUTE FUNCTION privacy_relay_forbid_mutation();
"#;

const DOWN: &str = r#"
DROP TABLE IF EXISTS privacy_relay_outbox;
DROP TABLE IF EXISTS relay_audit_events;
DROP TABLE IF EXISTS relay_authorization_decisions;
DROP FUNCTION IF EXISTS privacy_relay_safe_document(jsonb);
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
